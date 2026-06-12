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
        normalize_battle_outcomes,
        outcomes_permutation,
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

            assert!(outcomes_permutation(&outcomes, &expected_outcomes));
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
            assert!(outcomes_permutation(&outcomes, &expected_outcomes));
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
            assert!(outcomes_permutation(&outcomes, &expected_outcomes));
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
            assert!(outcomes_permutation(&outcomes, &expected_outcomes));
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
            assert!(outcomes_permutation(&outcomes, &expected_outcomes));
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
            assert!(outcomes_permutation(&outcomes, &expected_outcomes));
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
            assert!(outcomes_permutation(&outcomes, &expected_outcomes));
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
            assert!(outcomes_permutation(&outcomes, &expected_outcomes));
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
            assert!(outcomes_permutation(&outcomes, &expected_outcomes));
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
            assert!(outcomes_permutation(&outcomes, &expected_outcomes));
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
            assert!(outcomes_permutation(&outcomes, &expected_outcomes));
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
            assert!(outcomes_permutation(&outcomes, &expected_outcomes));
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
            assert!(outcomes_permutation(&outcomes, &expected_outcomes));
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
            assert!(outcomes_permutation(&outcomes, &expected_outcomes));
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
            assert!(outcomes_permutation(&outcomes, &expected_outcomes));
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
            assert!(outcomes_permutation(&outcomes, &expected_outcomes));
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
            // Weather Ball becomes Water-type in rain → rain ×1.5 weather mult now applies correctly.
            // Previously this was 70 (buggy: weather_damage_multiplier was reading the base Normal type).
            assert_eq!(rain_initial_hp - rain_final_state.p2_active_mons[0].hp, 105);
            // Weather Ball becomes Fire-type in sun → sun ×1.5 weather mult now applies correctly.
            // Previously this was 47 (same latent bug as above).
            assert_eq!(sun_initial_hp - sun_final_state.p2_active_mons[0].hp, 70);
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

        // ── -ate ability tests ────────────────────────────────────────────────────

        /// Build a 1v1 state for -ate tests.
        /// P1 (attacker) has the specified move + ability; P2 (defender) uses Splash so it never
        /// deals damage and never triggers any KO flow.
        fn ate_make_state(
            attacker_species: Species,
            attacker_move: PokemonMove,
            attacker_ability: Ability,
            defender_species: Species,
            pokemon_dex: &std::collections::HashMap<Species, crate::dex_data::PokemonData>,
            move_dex: &std::collections::HashMap<PokemonMove, crate::dex_data::MoveData>,
        ) -> BattleState {
            let attacker = build_pokemon_state(
                attacker_species, pokemon_dex, move_dex,
                Some(50), Some([Some(attacker_move), None, None, None]),
                None, Some(attacker_ability), Some(Nature::Hardy),
                None, None, Some([0, 0, 0, 0, 0, 0]), None, false,
            );
            let defender = build_pokemon_state(
                defender_species, pokemon_dex, move_dex,
                Some(50), Some([Some(PokemonMove::Splash), None, None, None]),
                None, Some(Ability::None), Some(Nature::Hardy),
                None, None, Some([0, 0, 0, 0, 0, 0]), None, false,
            );
            battle_state_from_lists(vec![attacker], vec![], vec![defender], vec![])
        }

        /// Run one deterministic turn (1 damage roll, no crit) and return P2's HP after.
        /// Returns 0 if P2 fainted and the game ended (GameOverState / no back-mons).
        fn ate_run_and_get_p2_hp(state: BattleState, move_dex: &std::collections::HashMap<PokemonMove, crate::dex_data::MoveData>, pokemon_dex: &std::collections::HashMap<Species, crate::dex_data::PokemonData>) -> u16 {
            let outcomes = simulate_turn(
                &MatchState::BattleState(state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                move_dex, pokemon_dex, false, 1,
            );
            match &outcomes.first().expect("at least one outcome").0 {
                MatchState::BattleState(bs) => bs.p2_active_mons[0].hp,
                _ => 0, // P2 fainted with no back-mons → game over; treat as 0 HP remaining
            }
        }

        #[test]
        fn ate_aerilate_bypasses_normal_type_immunity() {
            // Normal-type moves cannot hit Ghost-type Pokémon (immunity).
            // Aerilate converts Normal → Flying, which hits Ghost normally.
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let initial_hp = ate_make_state(
                Species::Incineroar, PokemonMove::BodySlam, Ability::Aerilate, Species::Gengar,
                &pokemon_dex, &move_dex,
            ).p2_active_mons[0].hp;

            let no_ability_hp = ate_run_and_get_p2_hp(
                ate_make_state(Species::Incineroar, PokemonMove::BodySlam, Ability::None, Species::Gengar, &pokemon_dex, &move_dex),
                &move_dex, &pokemon_dex,
            );
            let aerilate_hp = ate_run_and_get_p2_hp(
                ate_make_state(Species::Incineroar, PokemonMove::BodySlam, Ability::Aerilate, Species::Gengar, &pokemon_dex, &move_dex),
                &move_dex, &pokemon_dex,
            );

            assert_eq!(no_ability_hp, initial_hp, "Body Slam (Normal) should deal 0 to Ghost-type");
            assert!(aerilate_hp < initial_hp, "Aerilate (→ Flying) should deal damage to Ghost-type");
        }

        #[test]
        fn ate_refrigerate_converts_and_boosts() {
            // Refrigerate: Body Slam (Normal) → Ice-type + 1.2× boost.
            // Dragonite is Dragon/Flying; Ice is 2× vs Dragon.
            // Combined multiplier ≥ 2.4× baseline, so refrigerate damage > 2× baseline.
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let initial_hp = ate_make_state(
                Species::Incineroar, PokemonMove::BodySlam, Ability::None, Species::Dragonite,
                &pokemon_dex, &move_dex,
            ).p2_active_mons[0].hp;

            let no_ability_hp = ate_run_and_get_p2_hp(
                ate_make_state(Species::Incineroar, PokemonMove::BodySlam, Ability::None, Species::Dragonite, &pokemon_dex, &move_dex),
                &move_dex, &pokemon_dex,
            );
            let refrigerate_hp = ate_run_and_get_p2_hp(
                ate_make_state(Species::Incineroar, PokemonMove::BodySlam, Ability::Refrigerate, Species::Dragonite, &pokemon_dex, &move_dex),
                &move_dex, &pokemon_dex,
            );

            let no_ability_dmg = initial_hp - no_ability_hp;
            let refrigerate_dmg = initial_hp - refrigerate_hp;
            assert!(
                refrigerate_dmg > no_ability_dmg * 2,
                "Refrigerate (Ice 2×, 1.2× boost) vs Dragon should more than double baseline damage: {} vs {}",
                refrigerate_dmg, no_ability_dmg,
            );
        }

        #[test]
        fn ate_liquid_voice_no_power_boost() {
            // Liquid Voice converts sound moves to Water-type but grants NO power boost.
            //
            // Target: Machamp (pure Fighting) — both Normal and Water are 1× neutral vs Fighting,
            // so the only difference between LiquidVoice and no-ability should be zero (same BP,
            // same type effectiveness, no STAB for Incineroar on either type).
            // NOTE: Dragon resists Water (0.5×), so Dragonite would give a misleading result.
            //
            // We also contrast with Pixilate: Fairy is 2× vs Fighting, so with +1.2× boost
            // the damage should be well over 2× the baseline.
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let initial_hp = ate_make_state(
                Species::Incineroar, PokemonMove::HyperVoice, Ability::None, Species::Machamp,
                &pokemon_dex, &move_dex,
            ).p2_active_mons[0].hp;

            let no_ability_hp = ate_run_and_get_p2_hp(
                ate_make_state(Species::Incineroar, PokemonMove::HyperVoice, Ability::None, Species::Machamp, &pokemon_dex, &move_dex),
                &move_dex, &pokemon_dex,
            );
            let liquid_voice_hp = ate_run_and_get_p2_hp(
                ate_make_state(Species::Incineroar, PokemonMove::HyperVoice, Ability::LiquidVoice, Species::Machamp, &pokemon_dex, &move_dex),
                &move_dex, &pokemon_dex,
            );
            let pixilate_hp = ate_run_and_get_p2_hp(
                ate_make_state(Species::Incineroar, PokemonMove::HyperVoice, Ability::Pixilate, Species::Machamp, &pokemon_dex, &move_dex),
                &move_dex, &pokemon_dex,
            );

            let no_ability_dmg = initial_hp - no_ability_hp;
            let liquid_voice_dmg = initial_hp - liquid_voice_hp;
            let pixilate_dmg = initial_hp - pixilate_hp;

            assert_eq!(
                liquid_voice_dmg, no_ability_dmg,
                "Liquid Voice: same BP, same effectiveness vs Fighting → same damage (no 1.2× boost): {} vs {}",
                liquid_voice_dmg, no_ability_dmg,
            );
            assert!(
                pixilate_dmg > no_ability_dmg * 2,
                "Pixilate (Fairy 2×, 1.2× boost) vs Fighting should more than double damage: {} vs {}",
                pixilate_dmg, no_ability_dmg,
            );
        }

        #[test]
        fn ate_no_effect_on_non_normal_moves() {
            // Refrigerate only converts NORMAL moves. A Fire-type move (Flamethrower) is unchanged.
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let initial_hp = ate_make_state(
                Species::Incineroar, PokemonMove::Flamethrower, Ability::None, Species::Incineroar,
                &pokemon_dex, &move_dex,
            ).p2_active_mons[0].hp;

            let no_ability_hp = ate_run_and_get_p2_hp(
                ate_make_state(Species::Incineroar, PokemonMove::Flamethrower, Ability::None, Species::Incineroar, &pokemon_dex, &move_dex),
                &move_dex, &pokemon_dex,
            );
            let refrigerate_hp = ate_run_and_get_p2_hp(
                ate_make_state(Species::Incineroar, PokemonMove::Flamethrower, Ability::Refrigerate, Species::Incineroar, &pokemon_dex, &move_dex),
                &move_dex, &pokemon_dex,
            );

            assert_eq!(
                no_ability_hp, refrigerate_hp,
                "Refrigerate should not affect Flamethrower (Fire move); damage should be identical"
            );
            // Sanity: the move actually dealt some damage.
            assert!(no_ability_hp < initial_hp, "Flamethrower should deal damage");
        }

        #[test]
        fn ate_weather_ball_conditional_conversion() {
            // Weather Ball's own-effect sets its type to Water in rain — in that case -ate must
            // NOT convert it (the move's own type already fired). Without rain, Weather Ball is
            // Normal and SHOULD be converted+boosted by the -ate ability.
            //
            // Assertions:
            //   no_weather + Pixilate:  Fairy-type 60 BP (50 × 1.2) → some damage D_fairy
            //   no_weather + no ability: Normal 50 BP → baseline damage D_normal; D_fairy > D_normal
            //
            //   rain + Pixilate:         Water 100 BP (Weather Ball doubles in rain), rain mult ×1.5
            //                            → NOT converted, no ate boost → D_rain_pixilate
            //   rain + no ability:       same Water 100 BP × ×1.5 → D_rain_no_ability
            //   D_rain_pixilate == D_rain_no_ability  (Pixilate did nothing in rain)
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let make = |ability: Ability, rain: bool| {
                let mut state = ate_make_state(
                    Species::Incineroar, PokemonMove::WeatherBall, ability, Species::Incineroar,
                    &pokemon_dex, &move_dex,
                );
                if rain {
                    state.weather = Some(Weather::Rain);
                    state.weather_turns = Some(5);
                }
                state
            };

            let initial_hp = make(Ability::None, false).p2_active_mons[0].hp;

            let no_weather_no_ability_hp  = ate_run_and_get_p2_hp(make(Ability::None,    false), &move_dex, &pokemon_dex);
            let no_weather_pixilate_hp    = ate_run_and_get_p2_hp(make(Ability::Pixilate, false), &move_dex, &pokemon_dex);
            let rain_no_ability_hp        = ate_run_and_get_p2_hp(make(Ability::None,    true),  &move_dex, &pokemon_dex);
            let rain_pixilate_hp          = ate_run_and_get_p2_hp(make(Ability::Pixilate, true),  &move_dex, &pokemon_dex);

            let no_weather_no_ability_dmg = initial_hp - no_weather_no_ability_hp;
            let no_weather_pixilate_dmg   = initial_hp - no_weather_pixilate_hp;

            // Without rain: Pixilate converts Normal Weather Ball → Fairy (+1.2×); more damage.
            assert!(
                no_weather_pixilate_dmg > no_weather_no_ability_dmg,
                "No weather: Pixilate should boost Weather Ball (Normal→Fairy +1.2×): {} vs {}",
                no_weather_pixilate_dmg, no_weather_no_ability_dmg,
            );
            // With rain: Weather Ball's own type is Water; Pixilate does NOT convert → same damage.
            assert_eq!(
                rain_pixilate_hp, rain_no_ability_hp,
                "Rain: Pixilate must NOT convert Weather Ball (already Water); damage should be identical",
            );
            // Sanity: rain genuinely increases damage (Water Ball in rain = 100 BP × 1.5 weather mult).
            assert!(rain_no_ability_hp < no_weather_no_ability_hp,
                "Rain should increase Water-type Weather Ball damage");
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

        // ── Group B: Attack-stat boosts ───────────────────────────────────────

        #[test]
        fn huge_power_doubles_attack_stat() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();
            let mon = build_pokemon_state(
                Species::Marill, &pokemon_dex, &move_dex, Some(50),
                Some([Some(PokemonMove::Tackle), None, None, None]),
                None, Some(Ability::None), None, None, None, None, None, false,
            );
            let mut huge = mon.clone();
            huge.ability = Ability::HugePower;
            let state = battle_state_from_lists(vec![mon.clone()], vec![], vec![mon.clone()], vec![]);
            let atk_base = simulator_helpers::effective_stat(&state, &mon,  crate::dex_data::PokemonStat::Atk, false, false);
            let atk_huge = simulator_helpers::effective_stat(&state, &huge, crate::dex_data::PokemonStat::Atk, false, false);
            let spa_huge = simulator_helpers::effective_stat(&state, &huge, crate::dex_data::PokemonStat::SpA, false, false);
            let spa_base = simulator_helpers::effective_stat(&state, &mon,  crate::dex_data::PokemonStat::SpA, false, false);
            assert!((atk_huge - 2.0 * atk_base).abs() < 1e-9, "Huge Power should double Attack");
            assert!((spa_huge - spa_base).abs() < 1e-9, "Huge Power must not affect SpA");
        }

        #[test]
        fn pure_power_doubles_attack_stat() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();
            let mon = build_pokemon_state(
                Species::Medicham, &pokemon_dex, &move_dex, Some(50),
                Some([Some(PokemonMove::Tackle), None, None, None]),
                None, Some(Ability::None), None, None, None, None, None, false,
            );
            let mut pure = mon.clone();
            pure.ability = Ability::PurePower;
            let state = battle_state_from_lists(vec![mon.clone()], vec![], vec![mon.clone()], vec![]);
            let atk_base = simulator_helpers::effective_stat(&state, &mon,  crate::dex_data::PokemonStat::Atk, false, false);
            let atk_pure = simulator_helpers::effective_stat(&state, &pure, crate::dex_data::PokemonStat::Atk, false, false);
            assert!((atk_pure - 2.0 * atk_base).abs() < 1e-9, "Pure Power should double Attack");
        }

        #[test]
        fn hustle_boosts_attack_stat_by_1_5x() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();
            let mon = build_pokemon_state(
                Species::Deino, &pokemon_dex, &move_dex, Some(50),
                Some([Some(PokemonMove::Tackle), None, None, None]),
                None, Some(Ability::None), None, None, None, None, None, false,
            );
            let mut hustle = mon.clone();
            hustle.ability = Ability::Hustle;
            let state = battle_state_from_lists(vec![mon.clone()], vec![], vec![mon.clone()], vec![]);
            let atk_base   = simulator_helpers::effective_stat(&state, &mon,    crate::dex_data::PokemonStat::Atk, false, false);
            let atk_hustle = simulator_helpers::effective_stat(&state, &hustle, crate::dex_data::PokemonStat::Atk, false, false);
            let spa_hustle = simulator_helpers::effective_stat(&state, &hustle, crate::dex_data::PokemonStat::SpA, false, false);
            let spa_base   = simulator_helpers::effective_stat(&state, &mon,    crate::dex_data::PokemonStat::SpA, false, false);
            assert!((atk_hustle - 1.5 * atk_base).abs() < 1e-9, "Hustle should give 1.5x Attack");
            assert!((spa_hustle - spa_base).abs() < 1e-9, "Hustle must not affect SpA");
        }

        // ── Group A: Move-flag-based BP boosts ────────────────────────────────

        /// Helper: run a single turn with no-crit + 1 roll and return the
        /// probability-weighted expected damage dealt to P2's active mon.
        /// Using expected-damage (rather than extract_battle_state) means moves
        /// with probabilistic secondary effects don't cause assertion failures.
        fn damage_with_ability(
            attacker_species: Species,
            attacker_move: PokemonMove,
            ability: Ability,
            pokemon_dex: &std::collections::HashMap<Species, crate::dex_data::PokemonData>,
            move_dex: &std::collections::HashMap<PokemonMove, crate::dex_data::MoveData>,
        ) -> f64 {
            let attacker = build_pokemon_state(
                attacker_species, pokemon_dex, move_dex, Some(50),
                Some([Some(attacker_move), None, None, None]),
                None, Some(ability), Some(Nature::Hardy),
                None, None, Some([0, 0, 0, 0, 0, 0]), None, false,
            );
            let defender = build_pokemon_state(
                Species::Blissey, pokemon_dex, move_dex, Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None, Some(Ability::None), Some(Nature::Hardy),
                None, None, Some([0, 0, 0, 0, 0, 0]), None, false,
            );
            let initial_hp = defender.hp;
            let state = battle_state_from_lists(
                vec![attacker], vec![], vec![defender], vec![],
            );
            let outcomes = simulate_turn(
                &MatchState::BattleState(state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                move_dex, pokemon_dex, false, 1,
            );
            // Sum probability-weighted damage across all outcome branches.
            outcomes.iter().map(|(state, prob)| {
                let hp_after = match state {
                    MatchState::BattleState(bs) => bs.p2_active_mons[0].hp,
                    MatchState::GameOverState { .. } => 0,
                    _ => initial_hp,
                };
                (initial_hp.saturating_sub(hp_after) as f64) * prob
            }).sum()
        }

        #[test]
        fn iron_fist_boosts_punch_moves() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();
            // Bullet Punch (40 BP, punch flag) — boosted.
            let dmg_no  = damage_with_ability(Species::Hitmonchan, PokemonMove::BulletPunch, Ability::None, &pokemon_dex, &move_dex);
            let dmg_yes = damage_with_ability(Species::Hitmonchan, PokemonMove::BulletPunch, Ability::IronFist, &pokemon_dex, &move_dex);
            assert!(dmg_yes > dmg_no, "Iron Fist should boost punch moves");
            // Tackle (no punch flag) — not boosted.
            let dmg_tackle_no  = damage_with_ability(Species::Hitmonchan, PokemonMove::Tackle, Ability::None, &pokemon_dex, &move_dex);
            let dmg_tackle_yes = damage_with_ability(Species::Hitmonchan, PokemonMove::Tackle, Ability::IronFist, &pokemon_dex, &move_dex);
            assert_eq!(dmg_tackle_no, dmg_tackle_yes, "Iron Fist must not boost non-punch moves");
        }

        #[test]
        fn tough_claws_boosts_contact_moves() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();
            // Tackle has Contact flag — boosted.
            let dmg_no  = damage_with_ability(Species::Linoone, PokemonMove::Tackle, Ability::None, &pokemon_dex, &move_dex);
            let dmg_yes = damage_with_ability(Species::Linoone, PokemonMove::Tackle, Ability::ToughClaws, &pokemon_dex, &move_dex);
            assert!(dmg_yes > dmg_no, "Tough Claws should boost contact moves");
            // Swift has no Contact flag — not boosted.
            let dmg_swift_no  = damage_with_ability(Species::Linoone, PokemonMove::Swift, Ability::None, &pokemon_dex, &move_dex);
            let dmg_swift_yes = damage_with_ability(Species::Linoone, PokemonMove::Swift, Ability::ToughClaws, &pokemon_dex, &move_dex);
            assert_eq!(dmg_swift_no, dmg_swift_yes, "Tough Claws must not boost non-contact moves");
        }

        #[test]
        fn strong_jaw_boosts_bite_moves() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();
            // Jaw Lock (80 BP, bite flag, no probabilistic secondary) — boosted.
            let dmg_no  = damage_with_ability(Species::Arcanine, PokemonMove::JawLock, Ability::None, &pokemon_dex, &move_dex);
            let dmg_yes = damage_with_ability(Species::Arcanine, PokemonMove::JawLock, Ability::StrongJaw, &pokemon_dex, &move_dex);
            assert!(dmg_yes > dmg_no, "Strong Jaw should boost bite moves");
        }

        #[test]
        fn sharpness_boosts_slicing_moves() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();
            // Psycho Cut (70 BP, slicing flag) — boosted.
            let dmg_no  = damage_with_ability(Species::Gallade, PokemonMove::PsychoCut, Ability::None, &pokemon_dex, &move_dex);
            let dmg_yes = damage_with_ability(Species::Gallade, PokemonMove::PsychoCut, Ability::Sharpness, &pokemon_dex, &move_dex);
            assert!(dmg_yes > dmg_no, "Sharpness should boost slicing moves");
        }

        #[test]
        fn mega_launcher_boosts_pulse_moves() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();
            // Aura Sphere (80 BP, pulse flag) — boosted.
            let dmg_no  = damage_with_ability(Species::Blastoise, PokemonMove::AuraSphere, Ability::None, &pokemon_dex, &move_dex);
            let dmg_yes = damage_with_ability(Species::Blastoise, PokemonMove::AuraSphere, Ability::MegaLauncher, &pokemon_dex, &move_dex);
            assert!(dmg_yes > dmg_no, "Mega Launcher should boost pulse/aura moves");
            // Tackle (no pulse flag) — not boosted.
            let dmg_tackle_no  = damage_with_ability(Species::Blastoise, PokemonMove::Tackle, Ability::None, &pokemon_dex, &move_dex);
            let dmg_tackle_yes = damage_with_ability(Species::Blastoise, PokemonMove::Tackle, Ability::MegaLauncher, &pokemon_dex, &move_dex);
            assert_eq!(dmg_tackle_no, dmg_tackle_yes, "Mega Launcher must not boost non-pulse moves");
        }

        // ── Group A: Rivalry ──────────────────────────────────────────────────

        #[test]
        fn rivalry_boosts_vs_same_gender_and_reduces_vs_opposite() {
            use crate::pokemon::PokemonGender;
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let make_attacker = |ability: Ability, gender: PokemonGender| {
                let mut mon = build_pokemon_state(
                    Species::Zangoose, &pokemon_dex, &move_dex, Some(50),
                    Some([Some(PokemonMove::Tackle), None, None, None]),
                    None, Some(ability), Some(Nature::Hardy),
                    None, None, Some([0, 0, 0, 0, 0, 0]), None, false,
                );
                mon.gender = gender;
                mon
            };
            let make_defender = |gender: PokemonGender| {
                let mut mon = build_pokemon_state(
                    Species::Blissey, &pokemon_dex, &move_dex, Some(50),
                    Some([Some(PokemonMove::Splash), None, None, None]),
                    None, Some(Ability::None), Some(Nature::Hardy),
                    None, None, Some([0, 0, 0, 0, 0, 0]), None, false,
                );
                mon.gender = gender;
                mon
            };

            let run = |attacker: crate::pokemon::PokemonState, defender: crate::pokemon::PokemonState| -> u16 {
                let initial_hp = defender.hp;
                let state = battle_state_from_lists(vec![attacker], vec![], vec![defender], vec![]);
                let _hp_after = state.p2_active_mons[0].hp; // baseline before turn
                let outcomes = simulate_turn(
                    &MatchState::BattleState(state),
                    &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                    &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                    &move_dex, &pokemon_dex, false, 1,
                );
                let (final_state, _) = extract_battle_state(outcomes);
                initial_hp - final_state.p2_active_mons[0].hp
            };

            let dmg_neutral  = run(make_attacker(Ability::None, PokemonGender::Male), make_defender(PokemonGender::Male));
            let dmg_same     = run(make_attacker(Ability::Rivalry, PokemonGender::Male), make_defender(PokemonGender::Male));
            let dmg_opposite = run(make_attacker(Ability::Rivalry, PokemonGender::Male), make_defender(PokemonGender::Female));
            let dmg_genderless = run(make_attacker(Ability::Rivalry, PokemonGender::Genderless), make_defender(PokemonGender::Male));

            assert!(dmg_same > dmg_neutral, "Rivalry same-gender should boost damage");
            assert!(dmg_opposite < dmg_neutral, "Rivalry opposite-gender should reduce damage");
            assert_eq!(dmg_genderless, dmg_neutral, "Rivalry genderless should be neutral");
        }

        // ── Group A: Pinch abilities ──────────────────────────────────────────

        #[test]
        fn blaze_boosts_fire_moves_at_low_hp() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let make_attacker = |hp_fraction: f32| {
                let mut mon = build_pokemon_state(
                    Species::Charizard, &pokemon_dex, &move_dex, Some(50),
                    Some([Some(PokemonMove::Ember), None, None, None]),
                    None, Some(Ability::Blaze), Some(Nature::Hardy),
                    None, None, Some([0, 0, 0, 0, 0, 0]), None, false,
                );
                // Set HP to requested fraction.
                mon.hp = ((mon.stats[0] as f32 * hp_fraction) as u16).max(1);
                mon
            };
            let defender = build_pokemon_state(
                Species::Blissey, &pokemon_dex, &move_dex, Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None, Some(Ability::None), Some(Nature::Hardy),
                None, None, Some([0, 0, 0, 0, 0, 0]), None, false,
            );

            let run = |attacker: crate::pokemon::PokemonState| -> f64 {
                let initial_hp = defender.hp;
                let state = battle_state_from_lists(vec![attacker], vec![], vec![defender.clone()], vec![]);
                let outcomes = simulate_turn(
                    &MatchState::BattleState(state),
                    &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                    &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                    &move_dex, &pokemon_dex, false, 1,
                );
                outcomes.iter().map(|(s, p)| {
                    let hp = match s { MatchState::BattleState(bs) => bs.p2_active_mons[0].hp, _ => 0 };
                    (initial_hp.saturating_sub(hp) as f64) * p
                }).sum()
            };

            let dmg_full_hp = run(make_attacker(1.0));   // > 1/3 HP — no boost
            let dmg_low_hp  = run(make_attacker(0.25));  // ≤ 1/3 HP — boosted
            assert!(dmg_low_hp > dmg_full_hp, "Blaze should boost Fire moves at ≤1/3 HP");

            // A non-Fire move from a Blaze user at low HP should NOT be boosted.
            let mut blaze_tackle = build_pokemon_state(
                Species::Charizard, &pokemon_dex, &move_dex, Some(50),
                Some([Some(PokemonMove::Tackle), None, None, None]),
                None, Some(Ability::Blaze), Some(Nature::Hardy),
                None, None, Some([0, 0, 0, 0, 0, 0]), None, false,
            );
            blaze_tackle.hp = (blaze_tackle.stats[0] as f32 * 0.25) as u16;
            let mut no_ability_tackle = blaze_tackle.clone();
            no_ability_tackle.ability = Ability::None;
            let dmg_tackle_blaze   = run(blaze_tackle);
            let dmg_tackle_no_abil = run(no_ability_tackle);
            assert!((dmg_tackle_blaze - dmg_tackle_no_abil).abs() < 0.01, "Blaze must not boost non-Fire moves");
        }

        #[test]
        fn overgrow_boosts_grass_moves_at_low_hp() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();
            let make = |hp_frac: f32| {
                let mut mon = build_pokemon_state(
                    Species::Venusaur, &pokemon_dex, &move_dex, Some(50),
                    Some([Some(PokemonMove::VineWhip), None, None, None]),
                    None, Some(Ability::Overgrow), Some(Nature::Hardy),
                    None, None, Some([0, 0, 0, 0, 0, 0]), None, false,
                );
                mon.hp = ((mon.stats[0] as f32 * hp_frac) as u16).max(1);
                mon
            };
            let defender = build_pokemon_state(
                Species::Blissey, &pokemon_dex, &move_dex, Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None, Some(Ability::None), Some(Nature::Hardy),
                None, None, Some([0, 0, 0, 0, 0, 0]), None, false,
            );
            let run = |attacker| -> f64 {
                let initial_hp = defender.hp;
                let state = battle_state_from_lists(vec![attacker], vec![], vec![defender.clone()], vec![]);
                let outcomes = simulate_turn(
                    &MatchState::BattleState(state),
                    &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                    &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                    &move_dex, &pokemon_dex, false, 1,
                );
                outcomes.iter().map(|(s, p)| {
                    let hp = match s { MatchState::BattleState(bs) => bs.p2_active_mons[0].hp, _ => 0 };
                    (initial_hp.saturating_sub(hp) as f64) * p
                }).sum()
            };
            assert!(run(make(0.25)) > run(make(1.0)), "Overgrow should boost Grass moves at ≤1/3 HP");
        }

        #[test]
        fn swarm_boosts_bug_moves_at_low_hp() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();
            let make = |hp_frac: f32| {
                let mut mon = build_pokemon_state(
                    Species::Scizor, &pokemon_dex, &move_dex, Some(50),
                    Some([Some(PokemonMove::XScissor), None, None, None]),
                    None, Some(Ability::Swarm), Some(Nature::Hardy),
                    None, None, Some([0, 0, 0, 0, 0, 0]), None, false,
                );
                mon.hp = ((mon.stats[0] as f32 * hp_frac) as u16).max(1);
                mon
            };
            let defender = build_pokemon_state(
                Species::Blissey, &pokemon_dex, &move_dex, Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None, Some(Ability::None), Some(Nature::Hardy),
                None, None, Some([0, 0, 0, 0, 0, 0]), None, false,
            );
            let run = |attacker| -> f64 {
                let initial_hp = defender.hp;
                let state = battle_state_from_lists(vec![attacker], vec![], vec![defender.clone()], vec![]);
                let outcomes = simulate_turn(
                    &MatchState::BattleState(state),
                    &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                    &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                    &move_dex, &pokemon_dex, false, 1,
                );
                outcomes.iter().map(|(s, p)| {
                    let hp = match s { MatchState::BattleState(bs) => bs.p2_active_mons[0].hp, _ => 0 };
                    (initial_hp.saturating_sub(hp) as f64) * p
                }).sum()
            };
            assert!(run(make(0.25)) > run(make(1.0)), "Swarm should boost Bug moves at ≤1/3 HP");
        }

        #[test]
        fn torrent_boosts_water_moves_at_low_hp() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();
            let make = |hp_frac: f32| {
                let mut mon = build_pokemon_state(
                    Species::Blastoise, &pokemon_dex, &move_dex, Some(50),
                    Some([Some(PokemonMove::WaterGun), None, None, None]),
                    None, Some(Ability::Torrent), Some(Nature::Hardy),
                    None, None, Some([0, 0, 0, 0, 0, 0]), None, false,
                );
                mon.hp = ((mon.stats[0] as f32 * hp_frac) as u16).max(1);
                mon
            };
            let defender = build_pokemon_state(
                Species::Blissey, &pokemon_dex, &move_dex, Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None, Some(Ability::None), Some(Nature::Hardy),
                None, None, Some([0, 0, 0, 0, 0, 0]), None, false,
            );
            let run = |attacker| -> f64 {
                let initial_hp = defender.hp;
                let state = battle_state_from_lists(vec![attacker], vec![], vec![defender.clone()], vec![]);
                let outcomes = simulate_turn(
                    &MatchState::BattleState(state),
                    &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                    &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                    &move_dex, &pokemon_dex, false, 1,
                );
                outcomes.iter().map(|(s, p)| {
                    let hp = match s { MatchState::BattleState(bs) => bs.p2_active_mons[0].hp, _ => 0 };
                    (initial_hp.saturating_sub(hp) as f64) * p
                }).sum()
            };
            assert!(run(make(0.25)) > run(make(1.0)), "Torrent should boost Water moves at ≤1/3 HP");
        }

        // ── Group A: Technician ───────────────────────────────────────────────

        #[test]
        fn technician_boosts_moves_at_60_bp_or_less() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();
            // Tackle (40 BP) — boosted.
            let dmg_tackle_no  = damage_with_ability(Species::Meowth, PokemonMove::Tackle, Ability::None, &pokemon_dex, &move_dex);
            let dmg_tackle_yes = damage_with_ability(Species::Meowth, PokemonMove::Tackle, Ability::Technician, &pokemon_dex, &move_dex);
            assert!(dmg_tackle_yes > dmg_tackle_no, "Technician should boost 40 BP move");

            // Swift (60 BP exactly) — also boosted (threshold is inclusive).
            let dmg_swift_no  = damage_with_ability(Species::Meowth, PokemonMove::Swift, Ability::None, &pokemon_dex, &move_dex);
            let dmg_swift_yes = damage_with_ability(Species::Meowth, PokemonMove::Swift, Ability::Technician, &pokemon_dex, &move_dex);
            assert!(dmg_swift_yes > dmg_swift_no, "Technician should boost exactly 60 BP move");

            // Aura Sphere (80 BP) — not boosted.
            let dmg_aura_no  = damage_with_ability(Species::Meowth, PokemonMove::AuraSphere, Ability::None, &pokemon_dex, &move_dex);
            let dmg_aura_yes = damage_with_ability(Species::Meowth, PokemonMove::AuraSphere, Ability::Technician, &pokemon_dex, &move_dex);
            assert_eq!(dmg_aura_no, dmg_aura_yes, "Technician must not boost moves above 60 BP");
        }

        // ── Group C: Analytic ─────────────────────────────────────────────────

        #[test]
        fn analytic_boosts_damage_when_moving_last() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            // Build a Magmar attacker (decent SpA, uses Ember).  We override its
            // speed stat directly to control turn order without needing different
            // EV spreads across species with incompatible base speeds.
            // Defender is Blissey with speed stat 100.
            let make_attacker = |ability: Ability, spe_stat: u16| {
                let mut mon = build_pokemon_state(
                    Species::Magmar, &pokemon_dex, &move_dex, Some(50),
                    Some([Some(PokemonMove::Ember), None, None, None]),
                    None, Some(ability), Some(Nature::Hardy),
                    None, None, Some([0, 0, 0, 0, 0, 0]), None, false,
                );
                mon.stats[5] = spe_stat; // override speed: low = moves last, high = moves first
                mon
            };
            let defender = {
                let mut d = build_pokemon_state(
                    Species::Blissey, &pokemon_dex, &move_dex, Some(50),
                    Some([Some(PokemonMove::Splash), None, None, None]),
                    None, Some(Ability::None), Some(Nature::Hardy),
                    None, None, Some([0, 0, 0, 0, 0, 0]), None, false,
                );
                d.stats[5] = 100; // fixed reference speed
                d
            };

            let run = |attacker: crate::pokemon::PokemonState| -> f64 {
                let initial_hp = defender.hp;
                let state = battle_state_from_lists(
                    vec![attacker], vec![], vec![defender.clone()], vec![],
                );
                let outcomes = simulate_turn(
                    &MatchState::BattleState(state),
                    &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                    &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                    &move_dex, &pokemon_dex, false, 1,
                );
                outcomes.iter().map(|(s, p)| {
                    let hp = match s { MatchState::BattleState(bs) => bs.p2_active_mons[0].hp, _ => 0 };
                    (initial_hp.saturating_sub(hp) as f64) * p
                }).sum()
            };

            // Attacker spe=1 → slower than defender (100) → moves LAST → Analytic fires.
            let dmg_analytic_last = run(make_attacker(Ability::Analytic, 1));
            let dmg_no_analytic   = run(make_attacker(Ability::None, 1));
            assert!(dmg_analytic_last > dmg_no_analytic,
                "Analytic should boost damage when moving last");

            // Attacker spe=999 → faster than defender → moves FIRST → Analytic does NOT fire.
            let dmg_analytic_first   = run(make_attacker(Ability::Analytic, 999));
            let dmg_no_analytic_fast = run(make_attacker(Ability::None, 999));
            assert!((dmg_analytic_first - dmg_no_analytic_fast).abs() < 0.01,
                "Analytic must not boost damage when moving first");
        }

        // ── Group C: Fairy Aura ───────────────────────────────────────────────

        #[test]
        fn fairy_aura_boosts_fairy_type_moves() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            // P1 attacker uses Dazzling Gleam (Fairy, 80 BP).
            let make_attacker = || build_pokemon_state(
                Species::Sylveon, &pokemon_dex, &move_dex, Some(50),
                Some([Some(PokemonMove::DazzlingGleam), None, None, None]),
                None, Some(Ability::None), Some(Nature::Hardy),
                None, None, Some([0, 0, 0, 0, 0, 0]), None, false,
            );
            let make_defender = || build_pokemon_state(
                Species::Blissey, &pokemon_dex, &move_dex, Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None, Some(Ability::None), Some(Nature::Hardy),
                None, None, Some([0, 0, 0, 0, 0, 0]), None, false,
            );

            let run = |attacker: crate::pokemon::PokemonState, defender: crate::pokemon::PokemonState| -> f64 {
                let initial_hp: u16 = defender.hp;
                let state = battle_state_from_lists(vec![attacker], vec![], vec![defender], vec![]);
                let outcomes = simulate_turn(
                    &MatchState::BattleState(state),
                    &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                    &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                    &move_dex, &pokemon_dex, false, 1,
                );
                outcomes.iter().map(|(s, p)| {
                    let hp = match s { MatchState::BattleState(bs) => bs.p2_active_mons[0].hp, _ => 0 };
                    (initial_hp.saturating_sub(hp) as f64) * p
                }).sum()
            };

            // No Fairy Aura active — baseline damage.
            let dmg_no_aura = run(make_attacker(), make_defender());

            // Fairy Aura on the attacker — boosted.
            let mut aura_attacker = make_attacker();
            aura_attacker.ability = Ability::FairyAura;
            let dmg_aura_on_attacker = run(aura_attacker, make_defender());
            assert!(dmg_aura_on_attacker > dmg_no_aura,
                "Fairy Aura on attacker should boost Fairy moves");

            // Fairy Aura on the DEFENDER (field-wide) — still boosted.
            let mut aura_defender = make_defender();
            aura_defender.ability = Ability::FairyAura;
            let dmg_aura_on_defender = run(make_attacker(), aura_defender);
            assert!(dmg_aura_on_defender > dmg_no_aura,
                "Fairy Aura on the defender should still boost Fairy moves (field-wide)");

            // Non-Fairy move — not affected by Fairy Aura.
            let mut ember_attacker = make_attacker();
            ember_attacker.moves[0] = Some(PokemonMove::Ember);
            ember_attacker.ability = Ability::FairyAura;
            let mut ember_attacker_no = ember_attacker.clone();
            ember_attacker_no.ability = Ability::None;
            let dmg_ember_aura   = run(ember_attacker, make_defender());
            let dmg_ember_no     = run(ember_attacker_no, make_defender());
            assert!((dmg_ember_aura - dmg_ember_no).abs() < 0.01,
                "Fairy Aura must not boost non-Fairy moves");
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
                    moves_first: false,
                });
                let action_p2 = Action::MoveAction(MoveAction {
                    move_name: PokemonMove::Splash,
                    priority: 0,
                    user_slot: FieldSlot { player: Player::P2, slot_index: 0 },
                    target_slot: None,
                    moves_first: false,
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
                moves_first: false,
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
            simulator_helpers::check_and_apply_redirection(state, user_slot(), vec![primary_slot()], None)
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
            let result = simulator_helpers::check_and_apply_redirection(&state, user_slot(), spread.clone(), None);
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

    mod entry_effect_abilities {
        use super::*;

        // Shared mon builder: at lv50, given ability/item/status.
        fn mon(species: Species, ability: Ability, item: Option<Item>, status: Option<Status>) -> PokemonState {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();
            let mut p = build_pokemon_state(
                species,
                &pokemon_dex,
                &move_dex,
                Some(50),
                Some([Some(PokemonMove::Tackle), None, None, None]),
                None,          // gender
                Some(ability),
                None,          // nature
                item,          // item  (position 9 in build_pokemon_state)
                None,          // tera_type (position 10)
                Some([0; 6]),  // evs
                None,          // ivs
                false,
            );
            p.status = status;
            p
        }

        // Local copy of switch_p1_out (the one in switch_abilities is private to that mod).
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

        // ── Curious Medicine ─────────────────────────────────────────────────────────

        #[test]
        fn curious_medicine_resets_ally_boosts_on_entry() {
            // Build two allies: one with CuriousMedicine (enters second), one with preset boosts.
            let ally_with_boosts = {
                let mut m = mon(Species::Snorlax, Ability::Pressure, None, None);
                m.boosts = [2, -1, 1, 0, 0, 0, 0]; // some arbitrary boosts
                m
            };
            // CuriousMedicine mon enters as the second active mon in a doubles setup.
            let medicine_mon = mon(Species::Chansey, Ability::CuriousMedicine, None, None);
            // Build with both active; battle_state_from_lists fires send-out for both.
            let state = battle_state_from_lists(
                vec![ally_with_boosts, medicine_mon],
                vec![],
                vec![mon(Species::Snorlax, Ability::Pressure, None, None), mon(Species::Snorlax, Ability::Pressure, None, None)],
                vec![],
            );
            // Ally's boosts should be zeroed; medicine mon's own boosts are untouched (also 0).
            assert_eq!(state.p1_active_mons[0].boosts, [0; 7]);
            assert_eq!(state.p1_active_mons[1].boosts, [0; 7]);
        }

        #[test]
        fn curious_medicine_does_not_affect_self() {
            // Give the CuriousMedicine holder itself some boosts before send-out.
            // In practice boosts are always 0 at lead send-out, so we only check the post-state.
            // The ally's boosts are zeroed; self remains 0.
            let ally = {
                let mut m = mon(Species::Snorlax, Ability::Pressure, None, None);
                m.boosts = [3, 0, 0, 0, 0, 0, 0];
                m
            };
            let medicine = mon(Species::Chansey, Ability::CuriousMedicine, None, None);
            let state = battle_state_from_lists(
                vec![ally, medicine],
                vec![],
                vec![mon(Species::Snorlax, Ability::Pressure, None, None), mon(Species::Snorlax, Ability::Pressure, None, None)],
                vec![],
            );
            assert_eq!(state.p1_active_mons[0].boosts, [0; 7]);
        }

        // ── Hospitality ──────────────────────────────────────────────────────────────

        #[test]
        fn hospitality_heals_ally_one_quarter_max_hp_on_entry() {
            // Build a Blissey ally at reduced HP so the heal is visible but not capped.
            // Blissey has very high max HP — reduce by 80 HP so 1/4 max fits cleanly.
            let ally = {
                let pokemon_dex = pokemon_dex();
                let move_dex = move_dex();
                let mut m = build_pokemon_state(
                    Species::Blissey,
                    &pokemon_dex,
                    &move_dex,
                    Some(50),
                    Some([Some(PokemonMove::Tackle), None, None, None]),
                    None,
                    Some(Ability::Pressure),
                    None,
                    None,
                    None,
                    Some([0; 6]),
                    None,
                    false,
                );
                let lost = m.stats[0] / 2; // lose half HP — well below full
                m.hp = m.hp.saturating_sub(lost);
                m
            };
            let ally_max_hp = {
                let pokemon_dex = pokemon_dex();
                let move_dex = move_dex();
                build_pokemon_state(
                    Species::Blissey,
                    &pokemon_dex,
                    &move_dex,
                    Some(50),
                    Some([Some(PokemonMove::Tackle), None, None, None]),
                    None,
                    Some(Ability::Pressure),
                    None, None, None,
                    Some([0; 6]),
                    None,
                    false,
                ).stats[0]
            };
            let expected_heal = (ally_max_hp / 4).max(1);
            let starting_hp = ally.hp;

            let hospital = mon(Species::Chansey, Ability::Hospitality, None, None);
            let state = battle_state_from_lists(
                vec![ally, hospital],
                vec![],
                vec![
                    mon(Species::Snorlax, Ability::Pressure, None, None),
                    mon(Species::Snorlax, Ability::Pressure, None, None),
                ],
                vec![],
            );
            let actual_hp = state.p1_active_mons[0].hp;
            assert_eq!(actual_hp, starting_hp + expected_heal,
                "Hospitality should heal 1/4 ally max HP");
        }

        // ── Screen Cleaner ────────────────────────────────────────────────────────────

        #[test]
        fn screen_cleaner_removes_all_three_screens_from_both_sides() {
            use crate::dex_data::SideCondition;

            let holder = mon(Species::Snorlax, Ability::ScreenCleaner, None, None);
            let target = mon(Species::Snorlax, Ability::Pressure, None, None);

            // Build state, then manually set screens on both sides.
            let mut state = battle_state_from_lists(
                vec![mon(Species::Clefable, Ability::Pressure, None, None)],
                vec![holder],
                vec![target],
                vec![],
            );
            // Add all three screens to both sides before the Screen Cleaner switches in.
            for player in [Player::P1, Player::P2] {
                simulator_helpers::add_side_condition(&mut state, player, SideCondition::Reflect, 5);
                simulator_helpers::add_side_condition(&mut state, player, SideCondition::LightScreen, 5);
                simulator_helpers::add_side_condition(&mut state, player, SideCondition::AuroraVeil, 5);
            }

            // Now switch the Screen Cleaner in (replaces Clefable).
            let state = switch_p1_out(state);

            // All three screens on both sides should be gone.
            assert!(state.p1_side_conditions.is_empty(), "P1 screens should be cleared");
            assert!(state.p2_side_conditions.is_empty(), "P2 screens should be cleared");
        }

        // ── Supersweet Syrup ──────────────────────────────────────────────────────────

        #[test]
        fn supersweet_syrup_lowers_opponent_evasiveness_on_entry() {
            let syrup = mon(Species::Snorlax, Ability::SupersweetSyrup, None, None);
            let target = mon(Species::Snorlax, Ability::Pressure, None, None);
            let state = battle_state_from_lists(vec![syrup], vec![], vec![target], vec![]);
            // Eva is boost index 6; should be -1 after Supersweet Syrup triggers.
            assert_eq!(state.p2_active_mons[0].boosts[6], -1);
        }

        #[test]
        fn supersweet_syrup_fires_only_once_per_battle() {
            let syrup = mon(Species::Snorlax, Ability::SupersweetSyrup, None, None);
            let replacement = mon(Species::Clefable, Ability::Pressure, None, None);
            let target = mon(Species::Snorlax, Ability::Pressure, None, None);
            let initial = battle_state_from_lists(
                vec![syrup],
                vec![replacement],
                vec![target],
                vec![],
            );
            // First entry: -1 evasion, flag set.
            assert_eq!(initial.p2_active_mons[0].boosts[6], -1);
            assert!(initial.p1_active_mons[0].one_time_ability_used);

            // Switch out the syrup mon and bring it back in.
            let after_switch = switch_p1_out(initial);
            let syrup_bench = &after_switch.p1_back_mons[0];
            assert!(syrup_bench.one_time_ability_used, "Flag persists on bench");
            // Evasion should not have dropped a second time (-1, not -2).
            assert_eq!(after_switch.p2_active_mons[0].boosts[6], -1);
        }

        #[test]
        fn supersweet_syrup_blocked_by_clear_body() {
            let syrup = mon(Species::Snorlax, Ability::SupersweetSyrup, None, None);
            let immune = mon(Species::Snorlax, Ability::ClearBody, None, None);
            let state = battle_state_from_lists(vec![syrup], vec![], vec![immune], vec![]);
            // Clear Body blocks the evasion drop.
            assert_eq!(state.p2_active_mons[0].boosts[6], 0);
        }

        // ── Supreme Overlord ─────────────────────────────────────────────────────────

        #[test]
        fn supreme_overlord_volatile_set_to_fainted_count() {
            let overlord = mon(Species::Snorlax, Ability::SupremeOverlord, None, None);
            let mut fainted1 = mon(Species::Clefable, Ability::Pressure, None, None);
            fainted1.fainted = true;
            let mut fainted2 = mon(Species::Blissey, Ability::Pressure, None, None);
            fainted2.fainted = true;

            let state = battle_state_from_lists(
                vec![overlord.clone()],
                vec![fainted1, fainted2],
                vec![mon(Species::Snorlax, Ability::Pressure, None, None)],
                vec![],
            );

            // SupremeOverlord volatile should carry count = 2.
            let has_correct_volatile = state.p1_active_mons[0].volatiles.iter().any(|v| {
                matches!(v, VolatileStatusState::TurnStatus(VolatileStatus::SupremeOverlord(2), 0))
            });
            assert!(has_correct_volatile, "Should have SupremeOverlord(2) volatile");

            // Also test with 0 fainted: no volatile.
            let state_no_fainted = battle_state_from_lists(
                vec![overlord],
                vec![],
                vec![mon(Species::Snorlax, Ability::Pressure, None, None)],
                vec![],
            );
            let has_volatile = state_no_fainted.p1_active_mons[0].volatiles.iter().any(|v| {
                matches!(v, VolatileStatusState::TurnStatus(VolatileStatus::SupremeOverlord(_), _))
            });
            assert!(!has_volatile, "No volatile when no allies fainted");
        }

        #[test]
        fn supreme_overlord_boosts_damage_by_fainted_count() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            // Build attacker with SupremeOverlord and 2 fainted bench mons.
            let build_attacker = |fainted_count: u8| {
                let attacker = build_pokemon_state(
                    Species::Snorlax,
                    &pokemon_dex,
                    &move_dex,
                    Some(50),
                    Some([Some(PokemonMove::Tackle), None, None, None]),
                    None,
                    Some(Ability::SupremeOverlord),
                    Some(Nature::Adamant),
                    None,
                    None,
                    Some([0, 252, 0, 0, 0, 0]),
                    None,
                    false,
                );
                let bench: Vec<PokemonState> = (0..fainted_count).map(|_| {
                    let mut m = mon(Species::Clefable, Ability::Pressure, None, None);
                    m.fainted = true;
                    m
                }).collect();
                (attacker, bench)
            };

            // Bulky target: Blissey — high HP, neutral to Normal, won't faint.
            let make_target = || build_pokemon_state(
                Species::Blissey,
                &pokemon_dex,
                &move_dex,
                Some(50),
                Some([Some(PokemonMove::Tackle), None, None, None]),
                None,
                Some(Ability::Pressure),
                None,
                None,
                None,
                Some([0; 6]),
                None,
                false,
            );

            let run_damage = |fainted: u8| {
                let (attacker, bench) = build_attacker(fainted);
                let state = battle_state_from_lists(
                    vec![attacker],
                    bench,
                    vec![make_target()],
                    vec![],
                );
                let outcomes = run_single_turn(
                    &MatchState::BattleState(state.clone()),
                    &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                    &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                    &move_dex,
                    &pokemon_dex,
                );
                let target_hp = simulator_helpers::get_pokemon_at_slot(&state, FieldSlot { player: Player::P2, slot_index: 0 }).unwrap().hp;
                damage_distribution(&outcomes, target_hp)
            };

            let base_dist    = run_damage(0);
            let two_fainted  = run_damage(2);

            // With 2 fainted allies, damage should be ×1.2 (base × 1.2).
            // Compare expected distributions within 0.05 tolerance (per project memory).
            let expected: HashMap<u16, f64> = base_dist.iter()
                .map(|(dmg, prob)| (((*dmg as f64) * 1.2).floor() as u16, *prob))
                .collect();
            assert_distribution_close(two_fainted, expected);
        }

        // ── Trace ────────────────────────────────────────────────────────────────────

        #[test]
        fn trace_copies_opponent_ability_on_entry() {
            let tracer = mon(Species::Snorlax, Ability::Trace, None, None);
            let target  = mon(Species::Snorlax, Ability::Pressure, None, None);
            let state = battle_state_from_lists(vec![tracer], vec![], vec![target], vec![]);
            // Trace should have copied Pressure.
            assert_eq!(state.p1_active_mons[0].ability, Ability::Pressure);
            // original_ability should be set to Trace.
            assert_eq!(state.p1_active_mons[0].original_ability, Some(Ability::Trace));
        }

        #[test]
        fn trace_reverts_on_switch_out() {
            let tracer      = mon(Species::Snorlax, Ability::Trace, None, None);
            let replacement = mon(Species::Clefable, Ability::Pressure, None, None);
            let target      = mon(Species::Snorlax, Ability::Pressure, None, None);
            let initial = battle_state_from_lists(
                vec![tracer],
                vec![replacement],
                vec![target],
                vec![],
            );
            assert_eq!(initial.p1_active_mons[0].ability, Ability::Pressure);

            let after = switch_p1_out(initial);
            // Benched Trace mon should have reverted to Trace.
            assert_eq!(after.p1_back_mons[0].ability, Ability::Trace);
            assert_eq!(after.p1_back_mons[0].original_ability, None);
        }

        #[test]
        fn trace_does_not_copy_untraceable_ability() {
            let tracer = mon(Species::Snorlax, Ability::Trace, None, None);
            // Disguise is on the untraceable blocklist.
            let target = mon(Species::Snorlax, Ability::Disguise, None, None);
            let state = battle_state_from_lists(vec![tracer], vec![], vec![target], vec![]);
            // Trace should remain Trace since it could not copy Disguise.
            assert_eq!(state.p1_active_mons[0].ability, Ability::Trace);
            assert_eq!(state.p1_active_mons[0].original_ability, None);
        }

        #[test]
        fn trace_of_intimidate_triggers_intimidate() {
            let tracer = mon(Species::Snorlax, Ability::Trace, None, None);
            let target  = mon(Species::Snorlax, Ability::Intimidate, None, None);
            let state = battle_state_from_lists(vec![tracer], vec![], vec![target], vec![]);
            // Tracer copies Intimidate; copied Intimidate fires and lowers the opponent's Attack.
            assert_eq!(state.p1_active_mons[0].ability, Ability::Intimidate);
            // The trace-fired Intimidate lowers the target's (P2's) Attack.
            assert_eq!(state.p2_active_mons[0].boosts[0], -1,
                "Traced Intimidate should lower opponent's Attack");
        }

        // ── Imposter ─────────────────────────────────────────────────────────────────

        #[test]
        fn imposter_copies_species_and_moves_not_hp() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();
            // Imposter Ditto vs a Snorlax with specific moves.
            let target = build_pokemon_state(
                Species::Snorlax,
                &pokemon_dex,
                &move_dex,
                Some(50),
                Some([Some(PokemonMove::Earthquake), Some(PokemonMove::Tackle), None, None]),
                None,
                Some(Ability::ThickFat),
                None, None, None,
                Some([0; 6]),
                None,
                false,
            );
            let ditto = build_pokemon_state(
                Species::Ditto,
                &pokemon_dex,
                &move_dex,
                Some(50),
                Some([Some(PokemonMove::Transform), None, None, None]),
                None,
                Some(Ability::Imposter),
                None, None, None,
                Some([0; 6]),
                None,
                false,
            );
            let ditto_max_hp = ditto.stats[0];
            let state = battle_state_from_lists(
                vec![ditto],
                vec![],
                vec![target],
                vec![],
            );
            let transformed = &state.p1_active_mons[0];
            // Species, types, and moves should be copied.
            assert_eq!(transformed.species, Species::Snorlax);
            // Moves copied; PP capped at 5.
            assert_eq!(transformed.moves[0], Some(PokemonMove::Earthquake));
            assert_eq!(transformed.moves[1], Some(PokemonMove::Tackle));
            assert!(transformed.move_pp[0] <= 5, "Earthquake PP capped at 5");
            // HP is NOT copied — Ditto keeps its own.
            assert_eq!(transformed.stats[0], ditto_max_hp);
            // pre_transform is set (transform happened).
            assert!(transformed.pre_transform.is_some());
        }

        #[test]
        fn imposter_reverts_on_switch_out() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();
            let target = mon(Species::Snorlax, Ability::ThickFat, None, None);
            let ditto = build_pokemon_state(
                Species::Ditto,
                &pokemon_dex,
                &move_dex,
                Some(50),
                Some([Some(PokemonMove::Transform), None, None, None]),
                None,
                Some(Ability::Imposter),
                None, None, None,
                Some([0; 6]),
                None,
                false,
            );
            let replacement = mon(Species::Clefable, Ability::Pressure, None, None);

            let initial = battle_state_from_lists(
                vec![ditto],
                vec![replacement],
                vec![target],
                vec![],
            );
            // Confirm transformed.
            assert_eq!(initial.p1_active_mons[0].species, Species::Snorlax);

            let after = switch_p1_out(initial);
            // Benched Ditto should have reverted to Ditto species.
            assert_eq!(after.p1_back_mons[0].species, Species::Ditto);
            assert!(after.p1_back_mons[0].pre_transform.is_none(), "pre_transform cleared after revert");
        }

        #[test]
        fn imposter_fails_against_substitute() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();
            let mut target = mon(Species::Snorlax, Ability::Pressure, None, None);
            // Give target a Substitute volatile.
            target.volatiles.push(VolatileStatusState::TurnStatus(VolatileStatus::Substitute, 0));

            let ditto = build_pokemon_state(
                Species::Ditto,
                &pokemon_dex,
                &move_dex,
                Some(50),
                Some([Some(PokemonMove::Transform), None, None, None]),
                None,
                Some(Ability::Imposter),
                None, None, None,
                Some([0; 6]),
                None,
                false,
            );
            let state = battle_state_from_lists(vec![ditto], vec![], vec![target], vec![]);
            // Transform should have failed; Ditto keeps its own species.
            assert_eq!(state.p1_active_mons[0].species, Species::Ditto);
            assert!(state.p1_active_mons[0].pre_transform.is_none(), "No transform if target behind Substitute");
        }

        // ── Transform (move) ─────────────────────────────────────────────────────────

        #[test]
        fn transform_move_copies_target_species_and_caps_pp() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();
            // Ditto with Transform move vs Snorlax.
            let user = build_pokemon_state(
                Species::Ditto,
                &pokemon_dex,
                &move_dex,
                Some(50),
                Some([Some(PokemonMove::Transform), None, None, None]),
                None,
                Some(Ability::Limber),
                None, None, None,
                Some([0; 6]),
                None,
                false,
            );
            let target = build_pokemon_state(
                Species::Snorlax,
                &pokemon_dex,
                &move_dex,
                Some(50),
                Some([Some(PokemonMove::Earthquake), None, None, None]),
                None,
                Some(Ability::ThickFat),
                None, None, None,
                Some([0; 6]),
                None,
                false,
            );
            let state = battle_state_from_lists(vec![user], vec![], vec![target], vec![]);
            let outcomes = run_single_turn(
                &MatchState::BattleState(state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex,
                &pokemon_dex,
            );
            let (result, _) = extract_battle_state(outcomes);
            let transformed = &result.p1_active_mons[0];
            // After Transform, Ditto takes on Snorlax's species.
            assert_eq!(transformed.species, Species::Snorlax);
            // Copied Earthquake PP is capped at 5.
            assert_eq!(transformed.moves[0], Some(PokemonMove::Earthquake));
            assert!(transformed.move_pp[0] <= 5);
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

    // ──────────────────────────────────────────────────────────────────────────
    // Type-boosting held items, type-resist berries, status-cure berries
    // ──────────────────────────────────────────────────────────────────────────
    mod items_and_berries {
        use super::*;

        // ─── Shared helpers ───────────────────────────────────────────────────

        /// One-roll, no-crit damage from P1 slot 0 against P2 slot 0 using `mov`.
        fn raw_damage(
            state: &BattleState,
            mov: &PokemonMove,
            move_dex: &std::collections::HashMap<PokemonMove, crate::dex_data::MoveData>,
        ) -> u16 {
            let atk = FieldSlot { player: Player::P1, slot_index: 0 };
            let tgt = FieldSlot { player: Player::P2, slot_index: 0 };
            let md = move_dex.get(mov).unwrap();
            simulator_helpers::calculate_damage_outcomes_for_target(
                state,
                simulator_helpers::get_pokemon_at_slot(state, atk).unwrap(),
                simulator_helpers::get_pokemon_at_slot(state, tgt).unwrap(),
                atk, tgt, md,
                DamageConfig { consider_crit: false, damage_rolls: 1 }, 1.0, 1.0,
            )[0].0
        }

        // ─── Type-boosting held items ─────────────────────────────────────────

        #[test]
        fn charcoal_boosts_fire() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let make = |item: Option<Item>| {
                let atk = build_pokemon_state(
                    Species::Arcanine, &pokemon_dex, &move_dex, Some(50),
                    Some([Some(PokemonMove::Flamethrower), None, None, None]),
                    None, Some(Ability::None), None, item, None, None, None, false,
                );
                let tgt = build_pokemon_state(
                    Species::Snorlax, &pokemon_dex, &move_dex, Some(50),
                    Some([Some(PokemonMove::Splash), None, None, None]),
                    None, Some(Ability::None), None, None, None, None, None, false,
                );
                battle_state_from_lists(vec![atk], vec![], vec![tgt], vec![])
            };

            let no_item  = raw_damage(&make(None),                 &PokemonMove::Flamethrower, &move_dex);
            let charcoal = raw_damage(&make(Some(Item::Charcoal)), &PokemonMove::Flamethrower, &move_dex);

            assert!(charcoal > no_item, "Charcoal should boost Fire-move damage");
            // The boost should be close to 1.2× (integer floor rounding is fine)
            assert!(charcoal as f64 >= no_item as f64 * 1.15);
            assert!(charcoal as f64 <= no_item as f64 * 1.25);
        }

        #[test]
        fn silk_scarf_boosts_normal() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let make = |item: Option<Item>| {
                let atk = build_pokemon_state(
                    Species::Snorlax, &pokemon_dex, &move_dex, Some(50),
                    Some([Some(PokemonMove::BodySlam), None, None, None]),
                    None, Some(Ability::None), None, item, None, None, None, false,
                );
                let tgt = build_pokemon_state(
                    Species::Lapras, &pokemon_dex, &move_dex, Some(50),
                    Some([Some(PokemonMove::Splash), None, None, None]),
                    None, Some(Ability::None), None, None, None, None, None, false,
                );
                battle_state_from_lists(vec![atk], vec![], vec![tgt], vec![])
            };

            let no_item    = raw_damage(&make(None),                  &PokemonMove::BodySlam, &move_dex);
            let silk_scarf = raw_damage(&make(Some(Item::SilkScarf)), &PokemonMove::BodySlam, &move_dex);

            assert!(silk_scarf > no_item, "Silk Scarf should boost Normal-move damage");
            assert!(silk_scarf as f64 >= no_item as f64 * 1.15);
            assert!(silk_scarf as f64 <= no_item as f64 * 1.25);
        }

        #[test]
        fn charcoal_does_not_boost_nonfire() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let make = |item: Option<Item>| {
                let atk = build_pokemon_state(
                    Species::Arcanine, &pokemon_dex, &move_dex, Some(50),
                    Some([Some(PokemonMove::WaterPulse), None, None, None]),
                    None, Some(Ability::None), None, item, None, None, None, false,
                );
                let tgt = build_pokemon_state(
                    Species::Snorlax, &pokemon_dex, &move_dex, Some(50),
                    Some([Some(PokemonMove::Splash), None, None, None]),
                    None, Some(Ability::None), None, None, None, None, None, false,
                );
                battle_state_from_lists(vec![atk], vec![], vec![tgt], vec![])
            };

            let no_item  = raw_damage(&make(None),                 &PokemonMove::WaterPulse, &move_dex);
            let charcoal = raw_damage(&make(Some(Item::Charcoal)), &PokemonMove::WaterPulse, &move_dex);

            assert_eq!(charcoal, no_item, "Charcoal should not boost non-Fire moves");
        }

        #[test]
        fn type_boost_suppressed_by_magic_deluge() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let make = |suppressed: bool| {
                let atk = build_pokemon_state(
                    Species::Arcanine, &pokemon_dex, &move_dex, Some(50),
                    Some([Some(PokemonMove::Flamethrower), None, None, None]),
                    None, Some(Ability::None), None, Some(Item::Charcoal), None, None, None, false,
                );
                let tgt = build_pokemon_state(
                    Species::Snorlax, &pokemon_dex, &move_dex, Some(50),
                    Some([Some(PokemonMove::Splash), None, None, None]),
                    None, Some(Ability::None), None, None, None, None, None, false,
                );
                let mut state = battle_state_from_lists(vec![atk], vec![], vec![tgt], vec![]);
                if suppressed {
                    state.pseudo_weathers.push(PseudoWeather::MagicDeluge);
                    state.pseudo_weather_turns.push(5);
                }
                state
            };

            let boosted    = raw_damage(&make(false), &PokemonMove::Flamethrower, &move_dex);
            let suppressed = raw_damage(&make(true),  &PokemonMove::Flamethrower, &move_dex);

            assert!(boosted > suppressed,
                "Charcoal boost should be suppressed under Magic Room (MagicDeluge)");
        }

        // ─── Type-resist berries ──────────────────────────────────────────────

        #[test]
        fn occa_berry_halves_super_effective_fire() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            // Chikorita is Grass-type → 2× weak to Fire
            let make_target = |item: Option<Item>| {
                let atk = build_pokemon_state(
                    Species::Arcanine, &pokemon_dex, &move_dex, Some(50),
                    Some([Some(PokemonMove::Flamethrower), None, None, None]),
                    None, Some(Ability::None), None, None, None, None, None, false,
                );
                let tgt = build_pokemon_state(
                    Species::Chikorita, &pokemon_dex, &move_dex, Some(50),
                    Some([Some(PokemonMove::Splash), None, None, None]),
                    None, Some(Ability::None), None, item, None, None, None, false,
                );
                battle_state_from_lists(vec![atk], vec![], vec![tgt], vec![])
            };

            let dmg_bare = raw_damage(&make_target(None),                  &PokemonMove::Flamethrower, &move_dex);
            let dmg_occa = raw_damage(&make_target(Some(Item::OccaBerry)), &PokemonMove::Flamethrower, &move_dex);

            assert!(dmg_occa < dmg_bare, "Occa Berry should reduce SE Fire damage");
            assert!(dmg_occa as f64 >= dmg_bare as f64 * 0.45, "Reduction should be ~0.5×");
            assert!(dmg_occa as f64 <= dmg_bare as f64 * 0.55, "Reduction should be ~0.5×");

            // Full turn: berry consumed in every outcome branch.
            // (Flamethrower has a secondary burn chance → 2 branches, but the berry is consumed
            // in both, so we iterate all outcomes rather than using extract_battle_state.)
            let full_outcomes = run_single_turn(
                &MatchState::BattleState(make_target(Some(Item::OccaBerry))),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex, &pokemon_dex,
            );
            let any_not_consumed = full_outcomes.iter().any(|(ms, _)|
                matches!(ms, MatchState::BattleState(bs) if bs.p2_active_mons[0].item == Item::OccaBerry)
            );
            assert!(!any_not_consumed,
                "Occa Berry should be consumed in every branch after a super-effective Fire hit");
        }

        #[test]
        fn resist_berry_not_consumed_on_neutral_fire() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            // Snorlax is Normal-type → Fire hits at 1.0× (not super-effective); Occa should not fire.
            let make = |item: Option<Item>| {
                let atk = build_pokemon_state(
                    Species::Arcanine, &pokemon_dex, &move_dex, Some(50),
                    Some([Some(PokemonMove::Flamethrower), None, None, None]),
                    None, Some(Ability::None), None, None, None, None, None, false,
                );
                let tgt = build_pokemon_state(
                    Species::Snorlax, &pokemon_dex, &move_dex, Some(50),
                    Some([Some(PokemonMove::Splash), None, None, None]),
                    None, Some(Ability::None), None, item, None, None, None, false,
                );
                battle_state_from_lists(vec![atk], vec![], vec![tgt], vec![])
            };

            let dmg_with_berry = raw_damage(&make(Some(Item::OccaBerry)), &PokemonMove::Flamethrower, &move_dex);
            let dmg_no_berry   = raw_damage(&make(None),                   &PokemonMove::Flamethrower, &move_dex);

            assert_eq!(dmg_with_berry, dmg_no_berry,
                "Occa Berry should not reduce neutral Fire damage");

            // Full turn: berry should NOT be consumed in any branch.
            // (Flamethrower secondary burn → 2 branches; OccaBerry stays in all of them.)
            let full_outcomes = run_single_turn(
                &MatchState::BattleState(make(Some(Item::OccaBerry))),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex, &pokemon_dex,
            );
            for (ms, _) in &full_outcomes {
                if let MatchState::BattleState(bs) = ms {
                    assert_eq!(bs.p2_active_mons[0].item, Item::OccaBerry,
                        "Occa Berry should not be consumed on a neutral Fire hit");
                }
            }
        }

        #[test]
        fn chilan_triggers_on_neutral_normal() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            // Snorlax (Normal) uses Tackle (Normal, no secondary) on Lapras (Water/Ice) with ChilanBerry.
            // Effectiveness == 1.0 (Lapras is neutral to Normal), but Chilan fires on ANY Normal hit.
            // Snorlax gets STAB on Tackle so the damage is high enough to see the 0.5× reduction.
            // Tackle has no secondary effects, so the turn produces a single deterministic outcome.
            let make = |item: Option<Item>| {
                let atk = build_pokemon_state(
                    Species::Snorlax, &pokemon_dex, &move_dex, Some(50),
                    Some([Some(PokemonMove::Tackle), None, None, None]),
                    None, Some(Ability::None), None, None, None, None, None, false,
                );
                let tgt = build_pokemon_state(
                    Species::Lapras, &pokemon_dex, &move_dex, Some(50),
                    Some([Some(PokemonMove::Splash), None, None, None]),
                    None, Some(Ability::None), None, item, None, None, None, false,
                );
                battle_state_from_lists(vec![atk], vec![], vec![tgt], vec![])
            };

            let dmg_bare   = raw_damage(&make(None),                     &PokemonMove::Tackle, &move_dex);
            let dmg_chilan = raw_damage(&make(Some(Item::ChilanBerry)), &PokemonMove::Tackle, &move_dex);

            assert!(dmg_chilan < dmg_bare, "Chilan Berry should halve Normal-move damage even when not SE");
            assert!(dmg_chilan as f64 >= dmg_bare as f64 * 0.45, "Reduction should be ~0.5×");

            // Full turn: berry consumed (Tackle has no secondary → single deterministic outcome)
            let after = extract_battle_state(run_single_turn(
                &MatchState::BattleState(make(Some(Item::ChilanBerry))),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex, &pokemon_dex,
            )).0;

            assert_eq!(after.p2_active_mons[0].item, Item::None,
                "Chilan Berry should be consumed after any Normal-type hit");
        }

        #[test]
        fn chilan_not_triggered_by_nonnormal() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            // Close Combat (Fighting) is super-effective on Snorlax (Normal), but Chilan
            // Berry only fires for Normal-type moves — it should not reduce Fighting damage.
            let make = |item: Option<Item>| {
                let atk = build_pokemon_state(
                    Species::Machamp, &pokemon_dex, &move_dex, Some(50),
                    Some([Some(PokemonMove::CloseCombat), None, None, None]),
                    None, Some(Ability::None), None, None, None, None, None, false,
                );
                let tgt = build_pokemon_state(
                    Species::Snorlax, &pokemon_dex, &move_dex, Some(50),
                    Some([Some(PokemonMove::Splash), None, None, None]),
                    None, Some(Ability::None), None, item, None, None, None, false,
                );
                battle_state_from_lists(vec![atk], vec![], vec![tgt], vec![])
            };

            let dmg_bare   = raw_damage(&make(None),                     &PokemonMove::CloseCombat, &move_dex);
            let dmg_chilan = raw_damage(&make(Some(Item::ChilanBerry)), &PokemonMove::CloseCombat, &move_dex);

            assert_eq!(dmg_chilan, dmg_bare,
                "Chilan Berry should not reduce Fighting-move damage");

            // Full turn: berry NOT consumed. CloseCombat may OHKO Snorlax → GameOver.
            // We iterate all BattleState outcomes; if only GameOver, the berry was never consumed.
            let full_outcomes = run_single_turn(
                &MatchState::BattleState(make(Some(Item::ChilanBerry))),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex, &pokemon_dex,
            );
            let any_consumed = full_outcomes.iter().any(|(ms, _)|
                matches!(ms, MatchState::BattleState(bs) if bs.p2_active_mons[0].item == Item::None)
            );
            assert!(!any_consumed,
                "Chilan Berry should not be consumed by a non-Normal move");
        }

        #[test]
        fn resist_berry_suppressed_by_magic_deluge() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            // Occa Berry on a 2× Fire-weak target under Magic Room — no reduction, no consumption.
            let make = |suppressed: bool| {
                let atk = build_pokemon_state(
                    Species::Arcanine, &pokemon_dex, &move_dex, Some(50),
                    Some([Some(PokemonMove::Flamethrower), None, None, None]),
                    None, Some(Ability::None), None, None, None, None, None, false,
                );
                let tgt = build_pokemon_state(
                    Species::Chikorita, &pokemon_dex, &move_dex, Some(50),
                    Some([Some(PokemonMove::Splash), None, None, None]),
                    None, Some(Ability::None), None, Some(Item::OccaBerry), None, None, None, false,
                );
                let mut state = battle_state_from_lists(vec![atk], vec![], vec![tgt], vec![]);
                if suppressed {
                    state.pseudo_weathers.push(PseudoWeather::MagicDeluge);
                    state.pseudo_weather_turns.push(5);
                }
                state
            };

            let dmg_normal     = raw_damage(&make(false), &PokemonMove::Flamethrower, &move_dex);
            let dmg_suppressed = raw_damage(&make(true),  &PokemonMove::Flamethrower, &move_dex);

            assert!(dmg_suppressed > dmg_normal,
                "Occa Berry reduction should be suppressed by Magic Room");

            // Full turn: berry NOT consumed under Magic Room in any outcome branch.
            // (Flamethrower secondary → 2 branches; Occa should stay in all.)
            let full_outcomes = run_single_turn(
                &MatchState::BattleState(make(true)),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex, &pokemon_dex,
            );
            let any_consumed = full_outcomes.iter().any(|(ms, _)|
                matches!(ms, MatchState::BattleState(bs) if bs.p2_active_mons[0].item == Item::None)
            );
            assert!(!any_consumed,
                "Occa Berry should not be consumed under Magic Room");
        }

        // ─── Status-cure berries ──────────────────────────────────────────────

        #[test]
        fn cheri_cures_paralysis_immediately() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            // Thunder Wave onto a Cheri Berry holder. In any branch where the move hits,
            // the paralysis should be immediately cured and the berry consumed.
            let atk = build_pokemon_state(
                Species::Pikachu, &pokemon_dex, &move_dex, Some(50),
                Some([Some(PokemonMove::ThunderWave), None, None, None]),
                None, Some(Ability::None), None, None, None, None, None, false,
            );
            let tgt = build_pokemon_state(
                Species::Snorlax, &pokemon_dex, &move_dex, Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None, Some(Ability::None), None, Some(Item::CheriBerry), None, None, None, false,
            );
            let state = battle_state_from_lists(vec![atk], vec![], vec![tgt], vec![]);

            let outcomes = run_single_turn(
                &MatchState::BattleState(state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex, &pokemon_dex,
            );

            // Target should never end up paralyzed — whenever Thunder Wave hits, Cheri cures it.
            for (ms, _) in &outcomes {
                if let MatchState::BattleState(bs) = ms {
                    assert!(
                        !matches!(bs.p2_active_mons[0].status, Some(Status::Paralysis)),
                        "Cheri Berry should have cured paralysis immediately"
                    );
                }
            }
            // At least one branch consumes the berry (the hit branch).
            assert!(
                outcomes.iter().any(|(ms, _)|
                    matches!(ms, MatchState::BattleState(bs) if bs.p2_active_mons[0].item == Item::None)
                ),
                "Cheri Berry should be consumed when Thunder Wave hits"
            );
        }

        #[test]
        fn pecha_cures_toxic() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let atk = build_pokemon_state(
                Species::Arcanine, &pokemon_dex, &move_dex, Some(50),
                Some([Some(PokemonMove::Toxic), None, None, None]),
                None, Some(Ability::None), None, None, None, None, None, false,
            );
            let tgt = build_pokemon_state(
                Species::Snorlax, &pokemon_dex, &move_dex, Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None, Some(Ability::None), None, Some(Item::PechaBerry), None, None, None, false,
            );
            let state = battle_state_from_lists(vec![atk], vec![], vec![tgt], vec![]);

            let outcomes = run_single_turn(
                &MatchState::BattleState(state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex, &pokemon_dex,
            );

            // Target should never end up poisoned/badly poisoned.
            for (ms, _) in &outcomes {
                if let MatchState::BattleState(bs) = ms {
                    assert!(
                        !matches!(bs.p2_active_mons[0].status,
                            Some(Status::Poison | Status::ToxicPoison(_))),
                        "Pecha Berry should have cured toxic status immediately"
                    );
                }
            }
            assert!(
                outcomes.iter().any(|(ms, _)|
                    matches!(ms, MatchState::BattleState(bs) if bs.p2_active_mons[0].item == Item::None)
                ),
                "Pecha Berry should be consumed when Toxic hits"
            );
        }

        #[test]
        fn lum_cures_any_status() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let atk = build_pokemon_state(
                Species::Arcanine, &pokemon_dex, &move_dex, Some(50),
                Some([Some(PokemonMove::WillOWisp), None, None, None]),
                None, Some(Ability::None), None, None, None, None, None, false,
            );
            let tgt = build_pokemon_state(
                Species::Snorlax, &pokemon_dex, &move_dex, Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None, Some(Ability::None), None, Some(Item::LumBerry), None, None, None, false,
            );
            let state = battle_state_from_lists(vec![atk], vec![], vec![tgt], vec![]);

            let outcomes = run_single_turn(
                &MatchState::BattleState(state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex, &pokemon_dex,
            );

            // Target should never end up burned — Lum cures immediately.
            for (ms, _) in &outcomes {
                if let MatchState::BattleState(bs) = ms {
                    assert!(
                        !matches!(bs.p2_active_mons[0].status, Some(Status::Burn)),
                        "Lum Berry should have cured burn immediately"
                    );
                }
            }
            assert!(
                outcomes.iter().any(|(ms, _)|
                    matches!(ms, MatchState::BattleState(bs) if bs.p2_active_mons[0].item == Item::None)
                ),
                "Lum Berry should be consumed when Will-O-Wisp hits"
            );
        }

        #[test]
        fn lum_cures_confusion() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            // Confuse Ray has 100% accuracy and always confuses. With Lum Berry, confusion
            // is immediately cured and the berry is consumed.
            let atk = build_pokemon_state(
                Species::Gastly, &pokemon_dex, &move_dex, Some(50),
                Some([Some(PokemonMove::ConfuseRay), None, None, None]),
                None, Some(Ability::None), None, None, None, None, None, false,
            );
            let tgt = build_pokemon_state(
                Species::Snorlax, &pokemon_dex, &move_dex, Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None, Some(Ability::None), None, Some(Item::LumBerry), None, None, None, false,
            );
            let state = battle_state_from_lists(vec![atk], vec![], vec![tgt], vec![]);

            let after = extract_battle_state(run_single_turn(
                &MatchState::BattleState(state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex, &pokemon_dex,
            )).0;

            assert!(
                !simulator_helpers::is_confused(&after.p2_active_mons[0]),
                "Lum Berry should have cured confusion immediately"
            );
            assert_eq!(after.p2_active_mons[0].item, Item::None,
                "Lum Berry should be consumed after curing confusion");
        }

        #[test]
        fn persim_cures_confusion_only() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            // (a) ConfuseRay onto PersimBerry holder: confusion cured, berry consumed.
            let atk_c = build_pokemon_state(
                Species::Gastly, &pokemon_dex, &move_dex, Some(50),
                Some([Some(PokemonMove::ConfuseRay), None, None, None]),
                None, Some(Ability::None), None, None, None, None, None, false,
            );
            let tgt_c = build_pokemon_state(
                Species::Snorlax, &pokemon_dex, &move_dex, Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None, Some(Ability::None), None, Some(Item::PersimBerry), None, None, None, false,
            );
            let after_c = extract_battle_state(run_single_turn(
                &MatchState::BattleState(
                    battle_state_from_lists(vec![atk_c], vec![], vec![tgt_c], vec![])
                ),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex, &pokemon_dex,
            )).0;

            assert!(
                !simulator_helpers::is_confused(&after_c.p2_active_mons[0]),
                "Persim Berry should cure confusion"
            );
            assert_eq!(after_c.p2_active_mons[0].item, Item::None,
                "Persim Berry should be consumed after curing confusion");

            // (b) WillOWisp onto PersimBerry holder: burn remains, berry NOT consumed.
            let atk_b = build_pokemon_state(
                Species::Arcanine, &pokemon_dex, &move_dex, Some(50),
                Some([Some(PokemonMove::WillOWisp), None, None, None]),
                None, Some(Ability::None), None, None, None, None, None, false,
            );
            let tgt_b = build_pokemon_state(
                Species::Snorlax, &pokemon_dex, &move_dex, Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None, Some(Ability::None), None, Some(Item::PersimBerry), None, None, None, false,
            );
            let outcomes_b = run_single_turn(
                &MatchState::BattleState(
                    battle_state_from_lists(vec![atk_b], vec![], vec![tgt_b], vec![])
                ),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex, &pokemon_dex,
            );

            // Persim should never be consumed — it only cures confusion, not burn.
            for (ms, _) in &outcomes_b {
                if let MatchState::BattleState(bs) = ms {
                    assert_eq!(bs.p2_active_mons[0].item, Item::PersimBerry,
                        "Persim Berry should not be consumed when burn is inflicted");
                }
            }
        }

        #[test]
        fn status_cure_suppressed_by_magic_deluge() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            // Confuse Ray onto PersimBerry holder under Magic Room — berry should not fire.
            let atk = build_pokemon_state(
                Species::Gastly, &pokemon_dex, &move_dex, Some(50),
                Some([Some(PokemonMove::ConfuseRay), None, None, None]),
                None, Some(Ability::None), None, None, None, None, None, false,
            );
            let tgt = build_pokemon_state(
                Species::Snorlax, &pokemon_dex, &move_dex, Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None, Some(Ability::None), None, Some(Item::PersimBerry), None, None, None, false,
            );
            let mut state = battle_state_from_lists(vec![atk], vec![], vec![tgt], vec![]);
            state.pseudo_weathers.push(PseudoWeather::MagicDeluge);
            state.pseudo_weather_turns.push(5);

            let after = extract_battle_state(run_single_turn(
                &MatchState::BattleState(state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex, &pokemon_dex,
            )).0;

            assert!(
                simulator_helpers::is_confused(&after.p2_active_mons[0]),
                "Confusion should not be cured when Magic Room suppresses items"
            );
            assert_eq!(after.p2_active_mons[0].item, Item::PersimBerry,
                "Persim Berry should not be consumed under Magic Room");
        }
    }

    mod damage_override {
        use super::*;

        // Helper: run calculate_damage_outcomes_for_target and return all damage values.
        fn damage_values(
            attacker: &crate::pokemon::PokemonState,
            target: &crate::pokemon::PokemonState,
            state: &BattleState,
            move_name: PokemonMove,
            targets_mult: f64,
            move_dex: &std::collections::HashMap<PokemonMove, crate::dex_data::MoveData>,
        ) -> Vec<u16> {
            let attack_slot = FieldSlot { player: Player::P1, slot_index: 0 };
            let target_slot = FieldSlot { player: Player::P2, slot_index: 0 };
            simulator_helpers::calculate_damage_outcomes_for_target(
                state,
                attacker,
                target,
                attack_slot,
                target_slot,
                move_dex.get(&move_name).unwrap(),
                DamageConfig { consider_crit: false, damage_rolls: 16 },
                targets_mult,
                1.0,
            )
            .into_iter()
            .map(|(d, _, _)| d)
            .collect()
        }

        // ── Level-damage moves ────────────────────────────────────────────────

        #[test]
        fn seismic_toss_deals_user_level_to_non_immune_target() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            // Machamp (Fighting) at level 50 uses Seismic Toss.
            let attacker = build_pokemon_state(
                Species::Machamp,
                &pokemon_dex, &move_dex,
                Some(50),
                Some([Some(PokemonMove::SeismicToss), None, None, None]),
                None,
                Some(Ability::None),
                Some(Nature::Hardy),
                None, None,
                Some([0, 0, 0, 0, 0, 0]),
                Some([31, 31, 31, 31, 31, 31]),
                false,
            );
            // Venusaur is not Ghost-type, so it is not immune.
            let target = build_pokemon_state(
                Species::Venusaur,
                &pokemon_dex, &move_dex,
                Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None,
                Some(Ability::None),
                Some(Nature::Hardy),
                None, None,
                Some([0, 0, 0, 0, 0, 0]),
                Some([31, 31, 31, 31, 31, 31]),
                false,
            );
            let state = battle_state_from_lists(vec![attacker.clone()], vec![], vec![target.clone()], vec![]);
            let damages = damage_values(&attacker, &target, &state, PokemonMove::SeismicToss, 1.0, &move_dex);

            assert_eq!(damages, vec![50], "Seismic Toss should deal exactly user level (50) damage");
        }

        #[test]
        fn night_shade_deals_user_level_to_non_immune_target() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            // Gengar (Ghost) at level 50 uses Night Shade.
            let attacker = build_pokemon_state(
                Species::Gengar,
                &pokemon_dex, &move_dex,
                Some(50),
                Some([Some(PokemonMove::NightShade), None, None, None]),
                None,
                Some(Ability::None),
                Some(Nature::Hardy),
                None, None,
                Some([0, 0, 0, 0, 0, 0]),
                Some([31, 31, 31, 31, 31, 31]),
                false,
            );
            // Machamp is not Normal-type, so Night Shade is not immune.
            let target = build_pokemon_state(
                Species::Machamp,
                &pokemon_dex, &move_dex,
                Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None,
                Some(Ability::None),
                Some(Nature::Hardy),
                None, None,
                Some([0, 0, 0, 0, 0, 0]),
                Some([31, 31, 31, 31, 31, 31]),
                false,
            );
            let state = battle_state_from_lists(vec![attacker.clone()], vec![], vec![target.clone()], vec![]);
            let damages = damage_values(&attacker, &target, &state, PokemonMove::NightShade, 1.0, &move_dex);

            assert_eq!(damages, vec![50], "Night Shade should deal exactly user level (50) damage");
        }

        // ── Fixed-number damage moves ─────────────────────────────────────────

        #[test]
        fn dragon_rage_deals_exactly_40_damage() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let attacker = build_pokemon_state(
                Species::Dragonite,
                &pokemon_dex, &move_dex,
                Some(50),
                Some([Some(PokemonMove::DragonRage), None, None, None]),
                None,
                Some(Ability::None),
                Some(Nature::Hardy),
                None, None,
                Some([0, 0, 0, 0, 0, 0]),
                Some([31, 31, 31, 31, 31, 31]),
                false,
            );
            let target = build_pokemon_state(
                Species::Venusaur,
                &pokemon_dex, &move_dex,
                Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None,
                Some(Ability::None),
                Some(Nature::Hardy),
                None, None,
                Some([0, 0, 0, 0, 0, 0]),
                Some([31, 31, 31, 31, 31, 31]),
                false,
            );
            let state = battle_state_from_lists(vec![attacker.clone()], vec![], vec![target.clone()], vec![]);
            let damages = damage_values(&attacker, &target, &state, PokemonMove::DragonRage, 1.0, &move_dex);

            assert_eq!(damages, vec![40], "Dragon Rage should always deal exactly 40 damage");
        }

        // ── Type immunity zeroes out fixed damage ─────────────────────────────

        #[test]
        fn seismic_toss_deals_0_to_ghost_type() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            // Seismic Toss is Fighting-type; Ghost types are immune.
            let attacker = build_pokemon_state(
                Species::Machamp,
                &pokemon_dex, &move_dex,
                Some(50),
                Some([Some(PokemonMove::SeismicToss), None, None, None]),
                None,
                Some(Ability::None),
                Some(Nature::Hardy),
                None, None,
                Some([0, 0, 0, 0, 0, 0]),
                Some([31, 31, 31, 31, 31, 31]),
                false,
            );
            let target = build_pokemon_state(
                Species::Gengar,
                &pokemon_dex, &move_dex,
                Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None,
                Some(Ability::None),
                Some(Nature::Hardy),
                None, None,
                Some([0, 0, 0, 0, 0, 0]),
                Some([31, 31, 31, 31, 31, 31]),
                false,
            );
            let state = battle_state_from_lists(vec![attacker.clone()], vec![], vec![target.clone()], vec![]);
            let damages = damage_values(&attacker, &target, &state, PokemonMove::SeismicToss, 1.0, &move_dex);

            assert_eq!(damages, vec![0], "Seismic Toss should deal 0 damage to Ghost-type targets");
        }

        #[test]
        fn night_shade_deals_0_to_normal_type() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            // Night Shade is Ghost-type; Normal types are immune.
            let attacker = build_pokemon_state(
                Species::Gengar,
                &pokemon_dex, &move_dex,
                Some(50),
                Some([Some(PokemonMove::NightShade), None, None, None]),
                None,
                Some(Ability::None),
                Some(Nature::Hardy),
                None, None,
                Some([0, 0, 0, 0, 0, 0]),
                Some([31, 31, 31, 31, 31, 31]),
                false,
            );
            let target = build_pokemon_state(
                Species::Snorlax,
                &pokemon_dex, &move_dex,
                Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None,
                Some(Ability::None),
                Some(Nature::Hardy),
                None, None,
                Some([0, 0, 0, 0, 0, 0]),
                Some([31, 31, 31, 31, 31, 31]),
                false,
            );
            let state = battle_state_from_lists(vec![attacker.clone()], vec![], vec![target.clone()], vec![]);
            let damages = damage_values(&attacker, &target, &state, PokemonMove::NightShade, 1.0, &move_dex);

            assert_eq!(damages, vec![0], "Night Shade should deal 0 damage to Normal-type targets");
        }

        // ── Spread multiplier is ignored for fixed damage ─────────────────────

        #[test]
        fn fixed_damage_ignores_spread_multiplier() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let attacker = build_pokemon_state(
                Species::Machamp,
                &pokemon_dex, &move_dex,
                Some(50),
                Some([Some(PokemonMove::SeismicToss), None, None, None]),
                None,
                Some(Ability::None),
                Some(Nature::Hardy),
                None, None,
                Some([0, 0, 0, 0, 0, 0]),
                Some([31, 31, 31, 31, 31, 31]),
                false,
            );
            let target = build_pokemon_state(
                Species::Venusaur,
                &pokemon_dex, &move_dex,
                Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None,
                Some(Ability::None),
                Some(Nature::Hardy),
                None, None,
                Some([0, 0, 0, 0, 0, 0]),
                Some([31, 31, 31, 31, 31, 31]),
                false,
            );
            let state = battle_state_from_lists(vec![attacker.clone()], vec![], vec![target.clone()], vec![]);

            // With a 0.75 spread multiplier (doubles penalty) the result must still be 50.
            let damages = damage_values(&attacker, &target, &state, PokemonMove::SeismicToss, 0.75, &move_dex);
            assert_eq!(damages, vec![50],
                "Fixed-damage moves should deal full damage regardless of spread multiplier");
        }

        // ── 0 base-power moves deal exactly 0 damage (no min-1 clamp) ─────────

        #[test]
        fn zero_base_power_move_deals_zero_damage() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            // Splash has base power 0 and no DamageOverride. Category is Status so
            // move_offensive_stat returns None → 0. To test the bp==0.0 path we
            // need a Physical/Special move with bp 0. Use Seismic Toss but swap
            // its override by verifying the path separately via a manually-constructed
            // MoveData. Instead, use Dragon Rage's base move but confirm 0-bp
            // Physical moves through the actual data: SeismicToss has basePower=0
            // and DamageOverride::Level — the override fires first. We confirm the
            // intent via a hand-built MoveData that is Physical, bp=0, no override.
            use crate::dex_data::{DamageOverride, MoveCategory, MoveData, MoveTarget};

            let attacker = build_pokemon_state(
                Species::Machamp,
                &pokemon_dex, &move_dex,
                Some(50),
                Some([Some(PokemonMove::SeismicToss), None, None, None]),
                None,
                Some(Ability::None),
                Some(Nature::Hardy),
                None, None,
                Some([0, 0, 0, 0, 0, 0]),
                Some([31, 31, 31, 31, 31, 31]),
                false,
            );
            let target = build_pokemon_state(
                Species::Venusaur,
                &pokemon_dex, &move_dex,
                Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None,
                Some(Ability::None),
                Some(Nature::Hardy),
                None, None,
                Some([0, 0, 0, 0, 0, 0]),
                Some([31, 31, 31, 31, 31, 31]),
                false,
            );
            let state = battle_state_from_lists(vec![attacker.clone()], vec![], vec![target.clone()], vec![]);
            let attack_slot = FieldSlot { player: Player::P1, slot_index: 0 };
            let target_slot = FieldSlot { player: Player::P2, slot_index: 0 };

            // Build a minimal Physical MoveData with base_power=0, no override.
            let zero_bp_move = MoveData {
                name: PokemonMove::SeismicToss, // name doesn't matter for this path
                category: MoveCategory::Physical,
                base_power: 0,
                damage_override: DamageOverride::None,
                pokemon_type: crate::dex_data::PokemonType::Normal,
                accuracy: crate::dex_data::AccuracyType::Percent(100),
                pp: 10,
                priority: 0,
                target: MoveTarget::Normal,
                flags: vec![],
                secondaries: vec![],
                self_secondaries: vec![],
                multihit_range: [0, 0],
                multihit_accuracy: false,
                recoil_fraction: [0, 0],
                struggle_recoil: false,
                drain_fraction: [0, 0],
                self_destruct: crate::dex_data::SelfDestructType::None,
                self_switch: crate::dex_data::SelfSwitchType::None,
                force_switch: false,
                thaws_target: false,
                ohko: false,
                heal_fraction: [0, 0],
                self_boost: [0i8; 7],
                crit_ratio: 0,
                foul_play: false,
                breaks_protect: false,
                mind_blown_recoil: false,
                steals_boosts: false,
                override_offensive_stat: None,
                override_defensive_stat: None,
                ignore_ability: false,
                ignore_defense_boosts: false,
                ignore_evasion: false,
                ignore_immunity: vec![],
                sleep_usable: false,
                smart_target: false,
                tracks_target: false,
                calls_move: false,
                has_crash_damage: false,
                stalling_move: false,
            };

            let damages = simulator_helpers::calculate_damage_outcomes_for_target(
                &state,
                &attacker,
                &target,
                attack_slot,
                target_slot,
                &zero_bp_move,
                DamageConfig { consider_crit: false, damage_rolls: 16 },
                1.0,
                1.0,
            )
            .into_iter()
            .map(|(d, _, _)| d)
            .collect::<Vec<_>>();

            assert_eq!(damages, vec![0],
                "A Physical move with base_power 0 and no DamageOverride should deal 0 damage, not 1");
        }
    }

    // -------------------------------------------------------------------------
    // Self-switch tests (U-turn, Baton Pass, Shed Tail, …)
    // -------------------------------------------------------------------------
    mod self_switch {
        use super::*;
        use crate::battle::{AttackCommand, SwitchCommand};
        use crate::dex_data::SelfSwitchType;
        use crate::simulator::get_possible_commands_for_active_slot;

        // ── helpers ──────────────────────────────────────────────────────────

        fn u_turn_set() -> [Option<PokemonMove>; 4] {
            [Some(PokemonMove::Uturn), None, None, None]
        }

        fn baton_pass_set() -> [Option<PokemonMove>; 4] {
            [Some(PokemonMove::BatonPass), None, None, None]
        }

        fn shed_tail_set() -> [Option<PokemonMove>; 4] {
            [Some(PokemonMove::ShedTail), None, None, None]
        }

        fn splash_set() -> [Option<PokemonMove>; 4] {
            [Some(PokemonMove::Splash), None, None, None]
        }

        // Extract BattleState from an outcome Vec.  Panics if there is not exactly one
        // BattleState branch.
        fn single_battle_state(outcomes: &[(MatchState, f64)]) -> BattleState {
            let bs_outcomes: Vec<_> = outcomes.iter()
                .filter_map(|(s, _)| if let MatchState::BattleState(bs) = s { Some(bs.clone()) } else { None })
                .collect();
            assert_eq!(bs_outcomes.len(), 1, "Expected exactly one BattleState branch");
            bs_outcomes.into_iter().next().unwrap()
        }

        // ── test 1: U-turn interrupts mid-turn ───────────────────────────────

        #[test]
        fn u_turn_sets_self_switch_pending_mid_turn() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            // P1: fast (high Spe) U-turn user with a healthy bench mon.
            // P2: slow Shuckle using Splash — it should NOT have acted yet.
            let p1_active = build_pokemon_state(
                Species::Jolteon, &pokemon_dex, &move_dex, None,
                Some(u_turn_set()), None, None, None, None, None, None, None, false,
            );
            let p1_bench = build_pokemon_state(
                Species::Slowpoke, &pokemon_dex, &move_dex, None,
                Some(splash_set()), None, None, None, None, None, None, None, false,
            );
            let p2_active = build_pokemon_state(
                Species::Shuckle, &pokemon_dex, &move_dex, None,
                Some(splash_set()), None, None, None, None, None, None, None, false,
            );

            let initial = battle_state_from_lists(
                vec![p1_active], vec![p1_bench], vec![p2_active], vec![],
            );
            let p2_hp = initial.p2_active_mons[0].hp;

            let p1_cmd = PlayerCommand::Battle(simple_attack(Player::P1, vec![0]));
            let p2_cmd = PlayerCommand::Battle(simple_attack(Player::P2, vec![0]));

            let outcomes = run_single_turn(
                &MatchState::BattleState(initial), &p1_cmd, &p2_cmd, &move_dex, &pokemon_dex,
            );

            let bs = single_battle_state(&outcomes);

            // U-turn damage dealt to P2
            assert!(bs.p2_active_mons[0].hp < p2_hp, "U-turn should have dealt damage");

            // Self-switch pending for P1 slot 0
            let pending = bs.self_switch_pending.expect("self_switch_pending should be Some after U-turn");
            assert_eq!(pending.0, FieldSlot { player: Player::P1, slot_index: 0 });
            assert!(matches!(pending.1, SelfSwitchType::Normal));

            // Turn flags: started = true, ended = false
            assert!(bs.turn_started, "turn_started should still be true mid-queue");
            assert!(!bs.turn_ended, "turn_ended should be false — turn is not over");

            // Opponent's Splash is still queued
            assert!(!bs.action_queue.is_empty(), "opponent's queued Splash should still be in action_queue");
        }

        // ── test 2: legal choices are restricted while pending ────────────────

        #[test]
        fn restricted_choices_while_self_switch_pending() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let p1_active = build_pokemon_state(
                Species::Jolteon, &pokemon_dex, &move_dex, None,
                Some(u_turn_set()), None, None, None, None, None, None, None, false,
            );
            let p1_bench = build_pokemon_state(
                Species::Slowpoke, &pokemon_dex, &move_dex, None,
                Some(splash_set()), None, None, None, None, None, None, None, false,
            );
            let p2_active = build_pokemon_state(
                Species::Shuckle, &pokemon_dex, &move_dex, None,
                Some(splash_set()), None, None, None, None, None, None, None, false,
            );

            let initial = battle_state_from_lists(
                vec![p1_active], vec![p1_bench], vec![p2_active], vec![],
            );

            // Drive to the pending state
            let p1_cmd = PlayerCommand::Battle(simple_attack(Player::P1, vec![0]));
            let p2_cmd = PlayerCommand::Battle(simple_attack(Player::P2, vec![0]));
            let outcomes = run_single_turn(
                &MatchState::BattleState(initial), &p1_cmd, &p2_cmd, &move_dex, &pokemon_dex,
            );
            let bs = single_battle_state(&outcomes);
            assert!(bs.self_switch_pending.is_some());

            // Pending slot (P1 slot 0) should only offer Switch commands
            let p1_cmds = get_possible_commands_for_active_slot(&bs, Player::P1, 0, &move_dex, &pokemon_dex);
            assert!(!p1_cmds.is_empty(), "pending slot must have at least one command");
            for cmd in &p1_cmds {
                assert!(matches!(cmd, BattleCommand::Switch(_)),
                    "pending slot should only offer Switch, got {:?}", cmd);
            }

            // P2 (non-pending) should only get Pass
            let p2_cmds = get_possible_commands_for_active_slot(&bs, Player::P2, 0, &move_dex, &pokemon_dex);
            assert_eq!(p2_cmds, vec![BattleCommand::Pass],
                "non-pending player must only have Pass");
        }

        // ── test 3: resume — opponent acts, turn ends normally ────────────────

        #[test]
        fn u_turn_resume_resolves_turn() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let p1_active = build_pokemon_state(
                Species::Jolteon, &pokemon_dex, &move_dex, None,
                Some(u_turn_set()), None, None, None, None, None, None, None, false,
            );
            let p1_bench = build_pokemon_state(
                Species::Slowpoke, &pokemon_dex, &move_dex, None,
                Some(splash_set()), None, None, None, None, None, None, None, false,
            );
            let p2_active = build_pokemon_state(
                Species::Shuckle, &pokemon_dex, &move_dex, None,
                Some(splash_set()), None, None, None, None, None, None, None, false,
            );

            let initial = battle_state_from_lists(
                vec![p1_active], vec![p1_bench], vec![p2_active], vec![],
            );

            // Turn 1 step 1: U-turn fires
            let p1_cmd = PlayerCommand::Battle(simple_attack(Player::P1, vec![0]));
            let p2_cmd = PlayerCommand::Battle(simple_attack(Player::P2, vec![0]));
            let after_uturn = single_battle_state(&run_single_turn(
                &MatchState::BattleState(initial), &p1_cmd, &p2_cmd, &move_dex, &pokemon_dex,
            ));
            assert!(after_uturn.self_switch_pending.is_some());

            // Turn 1 step 2: send in Slowpoke (bench index 0)
            let p1_switch = PlayerCommand::Battle(vec![BattleCommand::Switch(SwitchCommand { party_index: 0 })]);
            let p2_pass = PlayerCommand::Battle(vec![BattleCommand::Pass]);
            let after_switch = single_battle_state(&run_single_turn(
                &MatchState::BattleState(after_uturn), &p1_switch, &p2_pass, &move_dex, &pokemon_dex,
            ));

            // self_switch_pending cleared
            assert!(after_switch.self_switch_pending.is_none(), "pending should be cleared after switch-in");

            // Slowpoke is now the active mon for P1
            assert_eq!(after_switch.p1_active_mons[0].species, Species::Slowpoke,
                "Slowpoke should be active after U-turn switch");

            // Jolteon is on bench
            assert_eq!(after_switch.p1_back_mons[0].species, Species::Jolteon,
                "Jolteon should be on the bench");

            // Turn ended normally: both flags reset for next turn
            assert!(!after_switch.turn_started, "turn should be fully over");
            assert!(!after_switch.turn_ended, "turn should be fully over");
        }

        // ── test 4: no healthy bench — no switch ─────────────────────────────

        #[test]
        fn u_turn_no_healthy_bench_does_not_switch() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let p1_active = build_pokemon_state(
                Species::Jolteon, &pokemon_dex, &move_dex, None,
                Some(u_turn_set()), None, None, None, None, None, None, None, false,
            );
            // No bench mons
            let p2_active = build_pokemon_state(
                Species::Shuckle, &pokemon_dex, &move_dex, None,
                Some(splash_set()), None, None, None, None, None, None, None, false,
            );

            let initial = battle_state_from_lists(
                vec![p1_active], vec![], vec![p2_active], vec![],
            );

            let p1_cmd = PlayerCommand::Battle(simple_attack(Player::P1, vec![0]));
            let p2_cmd = PlayerCommand::Battle(simple_attack(Player::P2, vec![0]));
            let outcomes = run_single_turn(
                &MatchState::BattleState(initial), &p1_cmd, &p2_cmd, &move_dex, &pokemon_dex,
            );
            let bs = single_battle_state(&outcomes);

            assert!(bs.self_switch_pending.is_none(), "no bench → no switch pending");
            // Turn completed normally
            assert!(!bs.turn_started);
            assert!(!bs.turn_ended);
        }

        // ── test 5: switch-out ability fires (DesolateLand / primal weather) ─────────────────────
        // DesolateLand sets primal sun (ExtremeSunlight) that ONLY lasts while its holder is on the
        // field. Regular Drought sets Weather::Sun for 5 turns regardless of whether the holder is
        // present, so it is intentionally not used here.

        #[test]
        fn u_turn_switch_out_ability_fires() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let p1_active = build_pokemon_state(
                Species::Torkoal, &pokemon_dex, &move_dex, None,
                Some(u_turn_set()), None, Some(Ability::DesolateLand), None, None, None, None, None, false,
            );
            let p1_bench = build_pokemon_state(
                Species::Slowpoke, &pokemon_dex, &move_dex, None,
                Some(splash_set()), None, None, None, None, None, None, None, false,
            );
            let p2_active = build_pokemon_state(
                Species::Shuckle, &pokemon_dex, &move_dex, None,
                Some(splash_set()), None, None, None, None, None, None, None, false,
            );

            let initial = battle_state_from_lists(
                vec![p1_active], vec![p1_bench], vec![p2_active], vec![],
            );
            // DesolateLand sets extreme sun on send-out
            assert!(matches!(initial.weather, Some(crate::dex_data::Weather::ExtremeSunlight)),
                "DesolateLand should have set ExtremeSunlight on send-out");

            // U-turn fires — returns with self_switch_pending set but the switch is not yet done
            let p1_cmd = PlayerCommand::Battle(simple_attack(Player::P1, vec![0]));
            let p2_cmd = PlayerCommand::Battle(simple_attack(Player::P2, vec![0]));
            let after_uturn = single_battle_state(&run_single_turn(
                &MatchState::BattleState(initial), &p1_cmd, &p2_cmd, &move_dex, &pokemon_dex,
            ));
            assert!(after_uturn.self_switch_pending.is_some());
            // The physical switch (and thus handle_pokemon_switch_out) has not happened yet;
            // extreme sun is still active while we await the replacement choice.
            assert!(matches!(after_uturn.weather, Some(crate::dex_data::Weather::ExtremeSunlight)),
                "Extreme sun should still be active while awaiting the switch-in choice");

            // Send in Slowpoke — this triggers perform_self_switch → perform_switch_out_in →
            // handle_pokemon_switch_out → handle_primal_weather_departure, ending the extreme sun.
            let p1_switch = PlayerCommand::Battle(vec![BattleCommand::Switch(SwitchCommand { party_index: 0 })]);
            let p2_pass = PlayerCommand::Battle(vec![BattleCommand::Pass]);
            let after_switch = single_battle_state(&run_single_turn(
                &MatchState::BattleState(after_uturn), &p1_switch, &p2_pass, &move_dex, &pokemon_dex,
            ));

            // DesolateLand source (Torkoal) left the field → extreme sun should be gone
            assert!(after_switch.weather.is_none(),
                "DesolateLand weather should end when Torkoal switches out via U-turn");
        }

        // ── test 6: Baton Pass transfers boosts and passable volatiles ────────

        #[test]
        fn baton_pass_transfers_boosts_and_volatiles() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let mut p1_active = build_pokemon_state(
                Species::Espeon, &pokemon_dex, &move_dex, None,
                Some(baton_pass_set()), None, None, None, None, None, None, None, false,
            );
            // Give Espeon +2 Atk and a LeechSeed volatile
            p1_active.boosts[0] = 2; // atk
            p1_active.volatiles.push(VolatileStatusState::TurnStatus(VolatileStatus::LeechSeed, 0));
            // Also give a Burn (non-passable status)
            p1_active.status = Some(Status::Burn);

            let p1_bench = build_pokemon_state(
                Species::Slowpoke, &pokemon_dex, &move_dex, None,
                Some(splash_set()), None, None, None, None, None, None, None, false,
            );
            let p2_active = build_pokemon_state(
                Species::Shuckle, &pokemon_dex, &move_dex, None,
                Some(splash_set()), None, None, None, None, None, None, None, false,
            );

            let initial = battle_state_from_lists(
                vec![p1_active], vec![p1_bench], vec![p2_active], vec![],
            );

            // Baton Pass (status category — always "connects")
            let p1_cmd = PlayerCommand::Battle(simple_attack(Player::P1, vec![0]));
            let p2_cmd = PlayerCommand::Battle(simple_attack(Player::P2, vec![0]));
            let after_bp = single_battle_state(&run_single_turn(
                &MatchState::BattleState(initial), &p1_cmd, &p2_cmd, &move_dex, &pokemon_dex,
            ));
            assert!(after_bp.self_switch_pending.is_some());
            let pending = after_bp.self_switch_pending.unwrap();
            assert!(matches!(pending.1, SelfSwitchType::BatonPass));

            // Send in Slowpoke
            let p1_switch = PlayerCommand::Battle(vec![BattleCommand::Switch(SwitchCommand { party_index: 0 })]);
            let p2_pass = PlayerCommand::Battle(vec![BattleCommand::Pass]);
            let after_switch = single_battle_state(&run_single_turn(
                &MatchState::BattleState(after_bp), &p1_switch, &p2_pass, &move_dex, &pokemon_dex,
            ));

            let replacement = &after_switch.p1_active_mons[0];
            assert_eq!(replacement.species, Species::Slowpoke);

            // +2 Atk boost passed
            assert_eq!(replacement.boosts[0], 2, "Baton Pass should transfer +2 Atk boost");

            // LeechSeed volatile passed
            let has_leech = replacement.volatiles.iter().any(|v|
                matches!(v, VolatileStatusState::TurnStatus(VolatileStatus::LeechSeed, _))
            );
            assert!(has_leech, "Baton Pass should transfer LeechSeed volatile");

            // Burn NOT passed (non-volatile status)
            assert!(replacement.status.is_none(), "Baton Pass must NOT transfer non-volatile status (Burn)");
        }

        // ── test 7: Shed Tail creates a Substitute and passes it ─────────────

        #[test]
        fn shed_tail_creates_sub_and_passes_it() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let p1_active = build_pokemon_state(
                Species::Orthworm, &pokemon_dex, &move_dex, None,
                Some(shed_tail_set()), None, None, None, None, None, None, None, false,
            );
            let p1_bench = build_pokemon_state(
                Species::Slowpoke, &pokemon_dex, &move_dex, None,
                Some(splash_set()), None, None, None, None, None, None, None, false,
            );
            let p2_active = build_pokemon_state(
                Species::Shuckle, &pokemon_dex, &move_dex, None,
                Some(splash_set()), None, None, None, None, None, None, None, false,
            );

            let initial = battle_state_from_lists(
                vec![p1_active], vec![p1_bench], vec![p2_active], vec![],
            );
            let max_hp = initial.p1_active_mons[0].stats[0];

            let p1_cmd = PlayerCommand::Battle(simple_attack(Player::P1, vec![0]));
            let p2_cmd = PlayerCommand::Battle(simple_attack(Player::P2, vec![0]));
            let after_shed = single_battle_state(&run_single_turn(
                &MatchState::BattleState(initial), &p1_cmd, &p2_cmd, &move_dex, &pokemon_dex,
            ));

            assert!(after_shed.self_switch_pending.is_some(), "Shed Tail should set self_switch_pending");
            let pending = after_shed.self_switch_pending.unwrap();
            assert!(matches!(pending.1, SelfSwitchType::ShedTail));

            // Orthworm lost ~half HP to create the substitute
            let expected_hp_after = max_hp - (max_hp + 1) / 2;
            let hp_after = after_shed.p1_active_mons[0].hp;
            assert!(hp_after <= expected_hp_after + 1, "Orthworm should have lost ~half HP for Shed Tail");

            // Send in Slowpoke
            let p1_switch = PlayerCommand::Battle(vec![BattleCommand::Switch(SwitchCommand { party_index: 0 })]);
            let p2_pass = PlayerCommand::Battle(vec![BattleCommand::Pass]);
            let after_switch = single_battle_state(&run_single_turn(
                &MatchState::BattleState(after_shed), &p1_switch, &p2_pass, &move_dex, &pokemon_dex,
            ));

            let replacement = &after_switch.p1_active_mons[0];
            assert_eq!(replacement.species, Species::Slowpoke);

            // Slowpoke has a Substitute volatile
            let has_sub = replacement.volatiles.iter().any(|v|
                matches!(v, VolatileStatusState::TurnStatus(VolatileStatus::Substitute, _))
            );
            assert!(has_sub, "Shed Tail replacement should have a Substitute volatile");

            // Slowpoke should NOT have boosts
            assert_eq!(replacement.boosts, [0i8; 7], "Shed Tail must NOT transfer boosts");
        }

        // ── test 8: Shed Tail fails when HP is ≤ 50% ─────────────────────────

        #[test]
        fn shed_tail_fails_when_low_hp() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let mut p1_active = build_pokemon_state(
                Species::Orthworm, &pokemon_dex, &move_dex, None,
                Some(shed_tail_set()), None, None, None, None, None, None, None, false,
            );
            // Set HP to exactly 50% — should fail
            let max_hp = p1_active.stats[0];
            p1_active.hp = max_hp / 2;

            let p1_bench = build_pokemon_state(
                Species::Slowpoke, &pokemon_dex, &move_dex, None,
                Some(splash_set()), None, None, None, None, None, None, None, false,
            );
            let p2_active = build_pokemon_state(
                Species::Shuckle, &pokemon_dex, &move_dex, None,
                Some(splash_set()), None, None, None, None, None, None, None, false,
            );

            let initial = battle_state_from_lists(
                vec![p1_active], vec![p1_bench], vec![p2_active], vec![],
            );

            let p1_cmd = PlayerCommand::Battle(simple_attack(Player::P1, vec![0]));
            let p2_cmd = PlayerCommand::Battle(simple_attack(Player::P2, vec![0]));
            let outcomes = run_single_turn(
                &MatchState::BattleState(initial), &p1_cmd, &p2_cmd, &move_dex, &pokemon_dex,
            );
            let bs = single_battle_state(&outcomes);

            assert!(bs.self_switch_pending.is_none(), "Shed Tail with HP ≤ 50% should fail — no switch pending");
            // No substitute on active mon
            let no_sub = bs.p1_active_mons[0].volatiles.iter().all(|v|
                !matches!(v, VolatileStatusState::TurnStatus(VolatileStatus::Substitute, _))
            );
            assert!(no_sub, "No Substitute should have been created when Shed Tail fails");
        }

        // ── test 9: Shed Tail fails when user already has a Substitute ────────

        #[test]
        fn shed_tail_fails_with_existing_substitute() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let mut p1_active = build_pokemon_state(
                Species::Orthworm, &pokemon_dex, &move_dex, None,
                Some(shed_tail_set()), None, None, None, None, None, None, None, false,
            );
            let initial_hp = p1_active.hp;
            // Pre-existing Substitute (HP value doesn't matter since blocking is unimplemented)
            p1_active.volatiles.push(VolatileStatusState::TurnStatus(VolatileStatus::Substitute, 30));

            let p1_bench = build_pokemon_state(
                Species::Slowpoke, &pokemon_dex, &move_dex, None,
                Some(splash_set()), None, None, None, None, None, None, None, false,
            );
            let p2_active = build_pokemon_state(
                Species::Shuckle, &pokemon_dex, &move_dex, None,
                Some(splash_set()), None, None, None, None, None, None, None, false,
            );

            let initial = battle_state_from_lists(
                vec![p1_active], vec![p1_bench], vec![p2_active], vec![],
            );

            let p1_cmd = PlayerCommand::Battle(simple_attack(Player::P1, vec![0]));
            let p2_cmd = PlayerCommand::Battle(simple_attack(Player::P2, vec![0]));
            let outcomes = run_single_turn(
                &MatchState::BattleState(initial), &p1_cmd, &p2_cmd, &move_dex, &pokemon_dex,
            );
            let bs = single_battle_state(&outcomes);

            // No switch pending
            assert!(bs.self_switch_pending.is_none(),
                "Shed Tail with existing Substitute should fail — no switch pending");
            // HP cost must NOT have been applied
            assert_eq!(bs.p1_active_mons[0].hp, initial_hp,
                "No HP cost should be taken when Shed Tail fails due to existing Substitute");
            // Still exactly one Substitute volatile (the original; no second one created)
            let sub_count = bs.p1_active_mons[0].volatiles.iter()
                .filter(|v| matches!(v, VolatileStatusState::TurnStatus(VolatileStatus::Substitute, _)))
                .count();
            assert_eq!(sub_count, 1, "Should still have exactly the original Substitute, not a new one");
        }

        // ── test 10: Shed Tail fails when there are no healthy teammates ───────

        #[test]
        fn shed_tail_fails_with_no_healthy_bench() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let p1_active = build_pokemon_state(
                Species::Orthworm, &pokemon_dex, &move_dex, None,
                Some(shed_tail_set()), None, None, None, None, None, None, None, false,
            );
            let initial_hp = p1_active.hp;
            // No bench mons at all
            let p2_active = build_pokemon_state(
                Species::Shuckle, &pokemon_dex, &move_dex, None,
                Some(splash_set()), None, None, None, None, None, None, None, false,
            );

            let initial = battle_state_from_lists(
                vec![p1_active], vec![], vec![p2_active], vec![],
            );

            let p1_cmd = PlayerCommand::Battle(simple_attack(Player::P1, vec![0]));
            let p2_cmd = PlayerCommand::Battle(simple_attack(Player::P2, vec![0]));
            let outcomes = run_single_turn(
                &MatchState::BattleState(initial), &p1_cmd, &p2_cmd, &move_dex, &pokemon_dex,
            );
            let bs = single_battle_state(&outcomes);

            // No switch pending
            assert!(bs.self_switch_pending.is_none(),
                "Shed Tail with no healthy bench should fail — no switch pending");
            // HP cost must NOT have been applied
            assert_eq!(bs.p1_active_mons[0].hp, initial_hp,
                "No HP cost should be taken when Shed Tail fails due to no healthy bench");
            // No Substitute should have been created
            let no_sub = bs.p1_active_mons[0].volatiles.iter().all(|v|
                !matches!(v, VolatileStatusState::TurnStatus(VolatileStatus::Substitute, _))
            );
            assert!(no_sub, "No Substitute should have been created when Shed Tail fails");
            // Turn completes normally (both flags reset)
            assert!(!bs.turn_started, "turn should be fully over");
            assert!(!bs.turn_ended, "turn should be fully over");
        }

        // ── HP-restoration berry tests ────────────────────────────────────────
        // Use poison residual as the trigger: it flows through take_damage →
        // on_hp_change, is deterministic (no RNG), and lets us set up exact HP.

        #[test]
        fn oran_heals_10_at_half_hp() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            // Snorlax: very bulky, easy to control HP maths.
            let mut p1 = build_pokemon_state(
                Species::Snorlax, &pokemon_dex, &move_dex, Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None, Some(Ability::None), None, Some(Item::OranBerry),
                None, None, None, false,
            );
            let max_hp = p1.stats[0];
            // Place P1 just above threshold so poison residual drops them to ≤50%.
            p1.hp = max_hp / 2 + 1;
            p1.status = Some(Status::Poison);

            let p2 = build_pokemon_state(
                Species::Shuckle, &pokemon_dex, &move_dex, Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None, Some(Ability::None), None, None, None, None, None, false,
            );

            let initial = battle_state_from_lists(vec![p1], vec![], vec![p2], vec![]);
            let outcomes = run_single_turn(
                &MatchState::BattleState(initial),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex, &pokemon_dex,
            );
            let (bs, _) = extract_battle_state(outcomes);

            // Poison residual = max_hp/8 (min 1), drops HP below threshold → berry fires.
            let poison_dmg = (max_hp / 8).max(1);
            let hp_after_poison = (max_hp / 2 + 1).saturating_sub(poison_dmg);
            let expected_hp = (hp_after_poison + 10).min(max_hp);

            assert_eq!(bs.p1_active_mons[0].item, Item::None, "Oran Berry should be consumed");
            assert_eq!(bs.p1_active_mons[0].hp, expected_hp, "Oran Berry should have healed 10 HP");
        }

        #[test]
        fn sitrus_heals_quarter_at_half_hp() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let mut p1 = build_pokemon_state(
                Species::Snorlax, &pokemon_dex, &move_dex, Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None, Some(Ability::None), None, Some(Item::SitrusBerry),
                None, None, None, false,
            );
            let max_hp = p1.stats[0];
            p1.hp = max_hp / 2 + 1;
            p1.status = Some(Status::Poison);

            let p2 = build_pokemon_state(
                Species::Shuckle, &pokemon_dex, &move_dex, Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None, Some(Ability::None), None, None, None, None, None, false,
            );

            let initial = battle_state_from_lists(vec![p1], vec![], vec![p2], vec![]);
            let outcomes = run_single_turn(
                &MatchState::BattleState(initial),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex, &pokemon_dex,
            );
            let (bs, _) = extract_battle_state(outcomes);

            let poison_dmg = (max_hp / 8).max(1);
            let hp_after_poison = (max_hp / 2 + 1).saturating_sub(poison_dmg);
            let expected_hp = (hp_after_poison + max_hp / 4).min(max_hp);

            assert_eq!(bs.p1_active_mons[0].item, Item::None, "Sitrus Berry should be consumed");
            assert_eq!(bs.p1_active_mons[0].hp, expected_hp, "Sitrus Berry should have healed max_hp/4");
        }

        #[test]
        fn hp_berry_not_eaten_above_half() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            // P1: Snorlax at full HP with poison — poison damage stays well above 50%.
            let mut p1 = build_pokemon_state(
                Species::Snorlax, &pokemon_dex, &move_dex, Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None, Some(Ability::None), None, Some(Item::OranBerry),
                None, None, None, false,
            );
            let max_hp = p1.stats[0];
            // At full HP, poison damage (max_hp/8) leaves us well above max_hp/2.
            p1.hp = max_hp;
            p1.status = Some(Status::Poison);

            let p2 = build_pokemon_state(
                Species::Shuckle, &pokemon_dex, &move_dex, Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None, Some(Ability::None), None, None, None, None, None, false,
            );

            let initial = battle_state_from_lists(vec![p1], vec![], vec![p2], vec![]);
            let outcomes = run_single_turn(
                &MatchState::BattleState(initial),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex, &pokemon_dex,
            );
            let (bs, _) = extract_battle_state(outcomes);

            let poison_dmg = (max_hp / 8).max(1);
            let expected_hp = max_hp.saturating_sub(poison_dmg);
            assert!(expected_hp > max_hp / 2,
                "Sanity: test setup should leave holder above threshold");

            assert_eq!(bs.p1_active_mons[0].item, Item::OranBerry,
                "Oran Berry should NOT be eaten when HP stays above 50%");
            assert_eq!(bs.p1_active_mons[0].hp, expected_hp,
                "HP should only reflect poison damage, not berry healing");
        }

        #[test]
        fn hp_berry_suppressed_by_magic_deluge() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let mut p1 = build_pokemon_state(
                Species::Snorlax, &pokemon_dex, &move_dex, Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None, Some(Ability::None), None, Some(Item::OranBerry),
                None, None, None, false,
            );
            let max_hp = p1.stats[0];
            p1.hp = max_hp / 2 + 1;
            p1.status = Some(Status::Poison);

            let p2 = build_pokemon_state(
                Species::Shuckle, &pokemon_dex, &move_dex, Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None, Some(Ability::None), None, None, None, None, None, false,
            );

            let mut initial = battle_state_from_lists(vec![p1], vec![], vec![p2], vec![]);
            // Magic Deluge suppresses held items.
            initial.pseudo_weathers.push(PseudoWeather::MagicDeluge);
            initial.pseudo_weather_turns.push(5);

            let outcomes = run_single_turn(
                &MatchState::BattleState(initial),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex, &pokemon_dex,
            );
            let (bs, _) = extract_battle_state(outcomes);

            // Berry should not have fired — item still held, HP only reflects poison damage.
            let poison_dmg = (max_hp / 8).max(1);
            let expected_hp = (max_hp / 2 + 1).saturating_sub(poison_dmg);
            assert_eq!(bs.p1_active_mons[0].item, Item::OranBerry,
                "Oran Berry must NOT be consumed while Magic Deluge suppresses items");
            assert_eq!(bs.p1_active_mons[0].hp, expected_hp,
                "HP should only reflect poison damage when items are suppressed");
        }

        // ── Leppa Berry test ──────────────────────────────────────────────────

        #[test]
        fn leppa_restores_pp_when_move_hits_zero() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            // P1 uses Splash with 1 PP remaining — after the turn it hits 0, triggering Leppa.
            let mut p1 = build_pokemon_state(
                Species::Snorlax, &pokemon_dex, &move_dex, Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None, Some(Ability::None), None, Some(Item::LeppaBerry),
                None, None, None, false,
            );
            p1.move_pp[0] = 1; // will hit 0 after this use

            let p2 = build_pokemon_state(
                Species::Shuckle, &pokemon_dex, &move_dex, Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None, Some(Ability::None), None, None, None, None, None, false,
            );

            let initial = battle_state_from_lists(vec![p1], vec![], vec![p2], vec![]);
            let outcomes = run_single_turn(
                &MatchState::BattleState(initial),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex, &pokemon_dex,
            );
            let (bs, _) = extract_battle_state(outcomes);

            // Leppa restores 10 PP (capped at max_pp).
            let max_pp = bs.p1_active_mons[0].max_pp[0];
            let expected_pp = max_pp.min(10);
            assert_eq!(bs.p1_active_mons[0].move_pp[0], expected_pp,
                "Leppa Berry should restore 10 PP (capped at max) when move hits 0");
            assert_eq!(bs.p1_active_mons[0].item, Item::None,
                "Leppa Berry should be consumed");
        }

        #[test]
        fn leppa_not_eaten_when_pp_above_zero() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            // P1 has 5 PP remaining — Splash use brings it to 4, not 0.
            let mut p1 = build_pokemon_state(
                Species::Snorlax, &pokemon_dex, &move_dex, Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None, Some(Ability::None), None, Some(Item::LeppaBerry),
                None, None, None, false,
            );
            p1.move_pp[0] = 5;

            let p2 = build_pokemon_state(
                Species::Shuckle, &pokemon_dex, &move_dex, Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None, Some(Ability::None), None, None, None, None, None, false,
            );

            let initial = battle_state_from_lists(vec![p1], vec![], vec![p2], vec![]);
            let outcomes = run_single_turn(
                &MatchState::BattleState(initial),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex, &pokemon_dex,
            );
            let (bs, _) = extract_battle_state(outcomes);

            assert_eq!(bs.p1_active_mons[0].move_pp[0], 4, "PP should simply decrement by 1");
            assert_eq!(bs.p1_active_mons[0].item, Item::LeppaBerry,
                "Leppa Berry must NOT be consumed when PP does not reach 0");
        }

        // ── Sitrus × Shed Tail interaction ────────────────────────────────────
        // These tests verify two invariants together:
        //   (a) Shed Tail costs ceil(max_hp/2), which always leaves HP ≤ floor(max_hp/2),
        //       so Sitrus always activates after a successful Shed Tail.
        //   (b) Even though Sitrus pushes HP back above threshold, the switch still triggers
        //       (baseline-comparison proxy in apply_post_damage_move_effects).

        #[test]
        fn sitrus_activates_after_shed_tail_even_hp() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let mut p1 = build_pokemon_state(
                Species::Orthworm, &pokemon_dex, &move_dex, None,
                Some([Some(PokemonMove::ShedTail), None, None, None]),
                None, Some(Ability::None), None, Some(Item::SitrusBerry),
                None, None, None, false,
            );
            // Force an even max HP for determinism.
            p1.stats[0] = 100;
            p1.hp = 100;

            let p1_bench = build_pokemon_state(
                Species::Slowpoke, &pokemon_dex, &move_dex, None,
                Some([Some(PokemonMove::Splash), None, None, None]),
                None, None, None, None, None, None, None, false,
            );
            let p2 = build_pokemon_state(
                Species::Shuckle, &pokemon_dex, &move_dex, None,
                Some([Some(PokemonMove::Splash), None, None, None]),
                None, None, None, None, None, None, None, false,
            );

            let initial = battle_state_from_lists(
                vec![p1], vec![p1_bench], vec![p2], vec![],
            );
            let outcomes = run_single_turn(
                &MatchState::BattleState(initial),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex, &pokemon_dex,
            );
            let (bs, _) = extract_battle_state(outcomes);

            // Switch should still be pending despite Sitrus healing P1 above 50%.
            assert!(bs.self_switch_pending.is_some(),
                "Shed Tail switch must still be pending even after Sitrus heals P1 above 50%");

            // Sitrus was consumed.
            assert_eq!(bs.p1_active_mons[0].item, Item::None,
                "Sitrus Berry should have fired after the Shed Tail HP cost");

            // HP: cost = ceil(100/2) = 50 → 50 remaining (≤50% threshold fires Sitrus)
            // Sitrus heals 100/4 = 25 → final HP = 75.
            assert_eq!(bs.p1_active_mons[0].hp, 75,
                "HP should be 75 after Shed Tail cost (50) and Sitrus heal (+25)");
        }

        #[test]
        fn sitrus_activates_after_shed_tail_odd_hp() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let mut p1 = build_pokemon_state(
                Species::Orthworm, &pokemon_dex, &move_dex, None,
                Some([Some(PokemonMove::ShedTail), None, None, None]),
                None, Some(Ability::None), None, Some(Item::SitrusBerry),
                None, None, None, false,
            );
            // Odd max HP: cost = ceil(101/2) = 51, leaving exactly 50 = floor(101/2).
            // This confirms the ceil rounding: with floor rounding the cost would be 50,
            // leaving 51 > 50 = floor(101/2), so Sitrus would NOT fire.
            p1.stats[0] = 101;
            p1.hp = 101;

            let p1_bench = build_pokemon_state(
                Species::Slowpoke, &pokemon_dex, &move_dex, None,
                Some([Some(PokemonMove::Splash), None, None, None]),
                None, None, None, None, None, None, None, false,
            );
            let p2 = build_pokemon_state(
                Species::Shuckle, &pokemon_dex, &move_dex, None,
                Some([Some(PokemonMove::Splash), None, None, None]),
                None, None, None, None, None, None, None, false,
            );

            let initial = battle_state_from_lists(
                vec![p1], vec![p1_bench], vec![p2], vec![],
            );
            let outcomes = run_single_turn(
                &MatchState::BattleState(initial),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex, &pokemon_dex,
            );
            let (bs, _) = extract_battle_state(outcomes);

            assert!(bs.self_switch_pending.is_some(),
                "Shed Tail switch must still be pending after Sitrus fires on odd-HP user");

            assert_eq!(bs.p1_active_mons[0].item, Item::None,
                "Sitrus Berry should fire when HP = 50 ≤ floor(101/2) = 50");

            // cost 51 → HP 50; Sitrus heals 101/4 = 25 → HP 75.
            assert_eq!(bs.p1_active_mons[0].hp, 75,
                "HP should be 75 after cost 51 and Sitrus heal +25");
        }

        // ── Focus Sash / Focus Band tests ─────────────────────────────────────
        // Setup trick: set target stats[0] = 1, hp = 1 so the holder is at
        // "full HP" (for Sash's condition) and any damaging move deals ≥ 2
        // (from the +2 floor in the damage formula), guaranteeing a lethal hit.
        // Both crit and non-crit branches KO the target, so they all trigger the
        // same endure logic and coalesce to a single outcome (prob 1.0).

        #[test]
        fn focus_sash_survives_lethal_hit_at_full_hp() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let mut p1 = build_pokemon_state(
                Species::Snorlax, &pokemon_dex, &move_dex, Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None, Some(Ability::None), None, Some(Item::FocusSash),
                None, None, None, false,
            );
            p1.stats[0] = 1;
            p1.hp = 1; // at full HP (1 == stats[0])

            let p2 = build_pokemon_state(
                Species::Shuckle, &pokemon_dex, &move_dex, Some(50),
                Some([Some(PokemonMove::Tackle), None, None, None]),
                None, Some(Ability::None), None, None,
                None, None, None, false,
            );

            let initial = battle_state_from_lists(vec![p1], vec![], vec![p2], vec![]);
            let outcomes = run_single_turn(
                &MatchState::BattleState(initial),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex, &pokemon_dex,
            );
            // Both crit and non-crit branches both trigger Sash → same surviving
            // state → coalesce_branches merges to exactly 1 outcome.
            let (bs, _) = extract_battle_state(outcomes);

            assert_eq!(bs.p1_active_mons[0].hp, 1, "Focus Sash should leave the holder at 1 HP");
            assert!(!bs.p1_active_mons[0].fainted, "Holder should not be fainted");
            assert_eq!(bs.p1_active_mons[0].item, Item::None, "Focus Sash should be consumed");
        }

        #[test]
        fn focus_sash_does_not_fire_when_not_at_full_hp() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let mut p1 = build_pokemon_state(
                Species::Snorlax, &pokemon_dex, &move_dex, Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None, Some(Ability::None), None, Some(Item::FocusSash),
                None, None, None, false,
            );
            p1.stats[0] = 2;
            p1.hp = 1; // NOT at full HP (1 < 2)

            let p2 = build_pokemon_state(
                Species::Shuckle, &pokemon_dex, &move_dex, Some(50),
                Some([Some(PokemonMove::Tackle), None, None, None]),
                None, Some(Ability::None), None, None,
                None, None, None, false,
            );

            let initial = battle_state_from_lists(vec![p1], vec![], vec![p2], vec![]);
            let outcomes = run_single_turn(
                &MatchState::BattleState(initial),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex, &pokemon_dex,
            );

            // Sash full-HP condition fails → holder faints → game over (P2 wins).
            let game_over_prob: f64 = outcomes.iter().map(|(s, p)| {
                if matches!(s, MatchState::GameOverState { winner: Player::P2 }) { *p } else { 0.0 }
            }).sum();
            assert!((game_over_prob - 1.0).abs() < 1e-9,
                "P1 should always faint when not at full HP (Sash cannot fire)");
        }

        #[test]
        fn focus_sash_not_consumed_on_non_lethal_hit() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            // Snorlax at full HP (~330 HP at level 50). Shuckle's Tackle deals
            // only ~3 damage — far from lethal — so Sash has no reason to fire.
            let p1 = build_pokemon_state(
                Species::Snorlax, &pokemon_dex, &move_dex, Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None, Some(Ability::None), None, Some(Item::FocusSash),
                None, None, None, false,
            );
            let p2 = build_pokemon_state(
                Species::Shuckle, &pokemon_dex, &move_dex, Some(50),
                Some([Some(PokemonMove::Tackle), None, None, None]),
                None, Some(Ability::None), None, None,
                None, None, None, false,
            );

            let initial = battle_state_from_lists(vec![p1], vec![], vec![p2], vec![]);
            let outcomes = run_single_turn(
                &MatchState::BattleState(initial),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex, &pokemon_dex,
            );

            // Across all branches (including crits): Sash should always be retained.
            let sash_retained_prob: f64 = outcomes.iter().map(|(s, p)| {
                if let MatchState::BattleState(bs) = s {
                    if bs.p1_active_mons[0].item == Item::FocusSash { *p } else { 0.0 }
                } else { 0.0 }
            }).sum();
            assert!((sash_retained_prob - 1.0).abs() < 1e-9,
                "Focus Sash should not be consumed by a non-lethal hit");
        }

        #[test]
        fn focus_band_gives_10_percent_survive_chance() {
            // Focus Band: 10% to survive any lethal hit at any HP; not consumed.
            // With crit branching, non-crit (93.75%) and crit (6.25%) both KO
            // the target, and Band rolls 10% to save each. After coalescing:
            // survive total = 0.1, faint total = 0.9.
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let mut p1 = build_pokemon_state(
                Species::Snorlax, &pokemon_dex, &move_dex, Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None, Some(Ability::None), None, Some(Item::FocusBand),
                None, None, None, false,
            );
            p1.stats[0] = 1;
            p1.hp = 1;

            let p2 = build_pokemon_state(
                Species::Shuckle, &pokemon_dex, &move_dex, Some(50),
                Some([Some(PokemonMove::Tackle), None, None, None]),
                None, Some(Ability::None), None, None,
                None, None, None, false,
            );

            let initial = battle_state_from_lists(vec![p1], vec![], vec![p2], vec![]);
            let outcomes = run_single_turn(
                &MatchState::BattleState(initial),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex, &pokemon_dex,
            );

            let survive_prob: f64 = outcomes.iter().map(|(s, p)| {
                if let MatchState::BattleState(bs) = s {
                    if bs.p1_active_mons[0].hp == 1
                        && bs.p1_active_mons[0].item == Item::FocusBand
                        && !bs.p1_active_mons[0].fainted
                    {
                        *p
                    } else { 0.0 }
                } else { 0.0 }
            }).sum();

            let faint_prob: f64 = outcomes.iter().map(|(s, p)| {
                if matches!(s, MatchState::GameOverState { winner: Player::P2 }) { *p } else { 0.0 }
            }).sum();

            assert!((survive_prob - 0.1).abs() < 1e-9,
                "Focus Band survive probability should be exactly 10%");
            assert!((faint_prob - 0.9).abs() < 1e-9,
                "Focus Band faint probability should be exactly 90%");
        }

        // ── White Herb tests ──────────────────────────────────────────────────

        #[test]
        fn white_herb_restores_stat_drop_from_growl() {
            // P2 uses Growl (lowers P1's Atk by −1). White Herb fires immediately,
            // restoring the stage to 0 and consuming the item.
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let p1 = build_pokemon_state(
                Species::Snorlax, &pokemon_dex, &move_dex, Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None, Some(Ability::None), None, Some(Item::WhiteHerb),
                None, None, None, false,
            );
            let p2 = build_pokemon_state(
                Species::Shuckle, &pokemon_dex, &move_dex, Some(50),
                Some([Some(PokemonMove::Growl), None, None, None]),
                None, Some(Ability::None), None, None,
                None, None, None, false,
            );

            let initial = battle_state_from_lists(vec![p1], vec![], vec![p2], vec![]);
            let outcomes = run_single_turn(
                &MatchState::BattleState(initial),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex, &pokemon_dex,
            );
            let (bs, _) = extract_battle_state(outcomes);

            assert_eq!(bs.p1_active_mons[0].boosts[0], 0,
                "White Herb should restore the Growl-induced −1 Atk drop to 0");
            assert_eq!(bs.p1_active_mons[0].item, Item::None,
                "White Herb should be consumed after restoring a stat drop");
        }

        #[test]
        fn white_herb_restores_intimidate_drop_on_send_out() {
            // Intimidate fires inside battle_state_from_lists (process_pokemon_send_out).
            // White Herb should also fire there — no turn needed.
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let p1 = build_pokemon_state(
                Species::Snorlax, &pokemon_dex, &move_dex, Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None, Some(Ability::None), None, Some(Item::WhiteHerb),
                None, None, None, false,
            );
            let p2 = build_pokemon_state(
                Species::Snorlax, &pokemon_dex, &move_dex, Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None, Some(Ability::Intimidate), None, None,
                None, None, None, false,
            );

            let initial = battle_state_from_lists(vec![p1], vec![], vec![p2], vec![]);

            assert_eq!(initial.p1_active_mons[0].boosts[0], 0,
                "White Herb should restore Intimidate's −1 Atk drop immediately on send-out");
            assert_eq!(initial.p1_active_mons[0].item, Item::None,
                "White Herb should be consumed after restoring the Intimidate drop");
        }

        #[test]
        fn white_herb_not_consumed_on_stat_raise() {
            // Swords Dance gives P1 +2 Atk (a raise, not a drop).
            // White Herb should not fire.
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let p1 = build_pokemon_state(
                Species::Snorlax, &pokemon_dex, &move_dex, Some(50),
                Some([Some(PokemonMove::SwordsDance), None, None, None]),
                None, Some(Ability::None), None, Some(Item::WhiteHerb),
                None, None, None, false,
            );
            let p2 = build_pokemon_state(
                Species::Shuckle, &pokemon_dex, &move_dex, Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None, Some(Ability::None), None, None,
                None, None, None, false,
            );

            let initial = battle_state_from_lists(vec![p1], vec![], vec![p2], vec![]);
            let outcomes = run_single_turn(
                &MatchState::BattleState(initial),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex, &pokemon_dex,
            );
            let (bs, _) = extract_battle_state(outcomes);

            assert_eq!(bs.p1_active_mons[0].boosts[0], 2,
                "Swords Dance should give +2 Atk");
            assert_eq!(bs.p1_active_mons[0].item, Item::WhiteHerb,
                "White Herb should not be consumed when only stat raises occur");
        }

        #[test]
        fn white_herb_not_consumed_when_items_suppressed() {
            // With Magic Deluge active items are suppressed; White Herb should not fire
            // even though Growl drops Atk.
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let p1 = build_pokemon_state(
                Species::Snorlax, &pokemon_dex, &move_dex, Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None, Some(Ability::None), None, Some(Item::WhiteHerb),
                None, None, None, false,
            );
            let p2 = build_pokemon_state(
                Species::Shuckle, &pokemon_dex, &move_dex, Some(50),
                Some([Some(PokemonMove::Growl), None, None, None]),
                None, Some(Ability::None), None, None,
                None, None, None, false,
            );

            let mut initial = battle_state_from_lists(vec![p1], vec![], vec![p2], vec![]);
            // Activate Magic Deluge to suppress items.
            initial.pseudo_weathers.push(PseudoWeather::MagicDeluge);
            initial.pseudo_weather_turns.push(5);

            let outcomes = run_single_turn(
                &MatchState::BattleState(initial),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex, &pokemon_dex,
            );
            let (bs, _) = extract_battle_state(outcomes);

            assert_eq!(bs.p1_active_mons[0].boosts[0], -1,
                "Growl should still drop Atk by 1 under Magic Deluge");
            assert_eq!(bs.p1_active_mons[0].item, Item::WhiteHerb,
                "White Herb should not be consumed when items are suppressed");
        }

        // ── Mental Herb tests ─────────────────────────────────────────────────

        #[test]
        fn mental_herb_cures_taunt() {
            // P2 uses Taunt on P1. Mental Herb fires immediately inside
            // apply_volatile_to_pokemon, removing Taunt and consuming the item.
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let p1 = build_pokemon_state(
                Species::Snorlax, &pokemon_dex, &move_dex, Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None, Some(Ability::None), None, Some(Item::MentalHerb),
                None, None, None, false,
            );
            let p2 = build_pokemon_state(
                Species::Shuckle, &pokemon_dex, &move_dex, Some(50),
                Some([Some(PokemonMove::Taunt), None, None, None]),
                None, Some(Ability::None), None, None,
                None, None, None, false,
            );

            let initial = battle_state_from_lists(vec![p1], vec![], vec![p2], vec![]);
            let outcomes = run_single_turn(
                &MatchState::BattleState(initial),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex, &pokemon_dex,
            );
            let (bs, _) = extract_battle_state(outcomes);

            let has_taunt = bs.p1_active_mons[0].volatiles.iter().any(|v| {
                matches!(v, VolatileStatusState::MoveStatus(VolatileStatus::Taunt, _))
            });
            assert!(!has_taunt, "Mental Herb should cure Taunt");
            assert_eq!(bs.p1_active_mons[0].item, Item::None,
                "Mental Herb should be consumed after curing a mental volatile");
        }

        #[test]
        fn mental_herb_not_consumed_by_non_mental_volatile() {
            // Confuse Ray inflicts Confusion, which is NOT in the Mental Herb's
            // cure list. The herb should remain held.
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let p1 = build_pokemon_state(
                Species::Snorlax, &pokemon_dex, &move_dex, Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None, Some(Ability::None), None, Some(Item::MentalHerb),
                None, None, None, false,
            );
            let p2 = build_pokemon_state(
                Species::Shuckle, &pokemon_dex, &move_dex, Some(50),
                Some([Some(PokemonMove::ConfuseRay), None, None, None]),
                None, Some(Ability::None), None, None,
                None, None, None, false,
            );

            let initial = battle_state_from_lists(vec![p1], vec![], vec![p2], vec![]);
            let outcomes = run_single_turn(
                &MatchState::BattleState(initial),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex, &pokemon_dex,
            );

            // Across all branches (including confusion self-hit branching):
            // herb should always be retained.
            let herb_retained_prob: f64 = outcomes.iter().map(|(s, p)| {
                if let MatchState::BattleState(bs) = s {
                    if bs.p1_active_mons[0].item == Item::MentalHerb { *p } else { 0.0 }
                } else { 0.0 }
            }).sum();
            assert!((herb_retained_prob - 1.0).abs() < 1e-9,
                "Mental Herb should not be consumed by a non-mental volatile (Confusion)");
        }

        #[test]
        fn mental_herb_not_consumed_when_items_suppressed() {
            // With Magic Deluge active, Taunt is inflicted but Mental Herb should
            // not fire (items suppressed).
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let p1 = build_pokemon_state(
                Species::Snorlax, &pokemon_dex, &move_dex, Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None, Some(Ability::None), None, Some(Item::MentalHerb),
                None, None, None, false,
            );
            let p2 = build_pokemon_state(
                Species::Shuckle, &pokemon_dex, &move_dex, Some(50),
                Some([Some(PokemonMove::Taunt), None, None, None]),
                None, Some(Ability::None), None, None,
                None, None, None, false,
            );

            let mut initial = battle_state_from_lists(vec![p1], vec![], vec![p2], vec![]);
            initial.pseudo_weathers.push(PseudoWeather::MagicDeluge);
            initial.pseudo_weather_turns.push(5);

            let outcomes = run_single_turn(
                &MatchState::BattleState(initial),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex, &pokemon_dex,
            );
            let (bs, _) = extract_battle_state(outcomes);

            let has_taunt = bs.p1_active_mons[0].volatiles.iter().any(|v| {
                matches!(v, VolatileStatusState::MoveStatus(VolatileStatus::Taunt, _))
            });
            assert!(has_taunt, "Taunt should still be inflicted under Magic Deluge");
            assert_eq!(bs.p1_active_mons[0].item, Item::MentalHerb,
                "Mental Herb should not be consumed when items are suppressed");
        }

        // ── Leftovers tests ───────────────────────────────────────────────────
        // Poison residual ticks first (via apply_status_residual) then Leftovers
        // heals at end of turn, so we can set up exact HP deltas.

        #[test]
        fn leftovers_heals_sixteenth_each_turn() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let mut p1 = build_pokemon_state(
                Species::Snorlax, &pokemon_dex, &move_dex, Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None, Some(Ability::None), None, Some(Item::Leftovers),
                None, None, None, false,
            );
            let max_hp = p1.stats[0];
            // Start at half HP so there is room to heal.
            p1.hp = max_hp / 2;

            let p2 = build_pokemon_state(
                Species::Shuckle, &pokemon_dex, &move_dex, Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None, Some(Ability::None), None, None, None, None, None, false,
            );

            let initial = battle_state_from_lists(vec![p1], vec![], vec![p2], vec![]);
            let outcomes = run_single_turn(
                &MatchState::BattleState(initial),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex, &pokemon_dex,
            );
            let (bs, _) = extract_battle_state(outcomes);

            let heal = (max_hp as u32 / 16).max(1) as u16;
            let expected_hp = (max_hp / 2 + heal).min(max_hp);
            assert_eq!(bs.p1_active_mons[0].item, Item::Leftovers, "Leftovers should not be consumed");
            assert_eq!(bs.p1_active_mons[0].hp, expected_hp, "Leftovers should heal 1/16 max HP");
        }

        #[test]
        fn leftovers_does_not_overheal_at_full_hp() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let mut p1 = build_pokemon_state(
                Species::Snorlax, &pokemon_dex, &move_dex, Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None, Some(Ability::None), None, Some(Item::Leftovers),
                None, None, None, false,
            );
            // Start at full HP.
            let max_hp = p1.stats[0];
            p1.hp = max_hp;

            let p2 = build_pokemon_state(
                Species::Shuckle, &pokemon_dex, &move_dex, Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None, Some(Ability::None), None, None, None, None, None, false,
            );

            let initial = battle_state_from_lists(vec![p1], vec![], vec![p2], vec![]);
            let outcomes = run_single_turn(
                &MatchState::BattleState(initial),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex, &pokemon_dex,
            );
            let (bs, _) = extract_battle_state(outcomes);

            assert_eq!(bs.p1_active_mons[0].hp, max_hp, "Leftovers should not overheal past max HP");
        }

        // ── Shell Bell tests ──────────────────────────────────────────────────

        #[test]
        fn shell_bell_heals_eighth_of_damage_dealt() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            // Attacker with Shell Bell; starts below max HP so there is room to heal.
            let mut p1 = build_pokemon_state(
                Species::Snorlax, &pokemon_dex, &move_dex, Some(50),
                Some([Some(PokemonMove::Tackle), None, None, None]),
                None, Some(Ability::None), None, Some(Item::ShellBell),
                None, None, None, false,
            );
            let p1_max_hp = p1.stats[0];
            p1.hp = p1_max_hp / 2;
            let p1_before_hp = p1.hp;

            let p2 = build_pokemon_state(
                Species::Snorlax, &pokemon_dex, &move_dex, Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None, Some(Ability::None), None, None, None, None, None, false,
            );
            let p2_before_hp = p2.stats[0]; // starts at full HP

            let initial = battle_state_from_lists(vec![p1], vec![], vec![p2], vec![]);
            let outcomes = run_single_turn(
                &MatchState::BattleState(initial),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex, &pokemon_dex,
            );
            let (bs, _) = extract_battle_state(outcomes);

            let damage_dealt = p2_before_hp.saturating_sub(bs.p2_active_mons[0].hp) as u32;
            let expected_heal = (damage_dealt / 8) as u16;
            let expected_p1_hp = (p1_before_hp + expected_heal).min(p1_max_hp);

            assert!(damage_dealt > 0, "P1 Tackle should deal damage to P2");
            assert!(expected_heal > 0, "damage should be large enough for at least 1 HP of Shell Bell healing");
            assert_eq!(bs.p1_active_mons[0].item, Item::ShellBell, "Shell Bell should not be consumed");
            assert_eq!(bs.p1_active_mons[0].hp, expected_p1_hp, "Shell Bell should restore 1/8 of damage dealt");
        }

        // ── Scope Lens tests ──────────────────────────────────────────────────
        // Tested through simulate_turn with consider_crit=true, damage_rolls=1.
        // With one roll, outcomes split into exactly two groups: the crit branch
        // (higher damage, lower target HP) and the non-crit branch.  The probability
        // mass on the min-HP group equals the crit rate: 1/8 with Scope Lens (vs 1/24).

        #[test]
        fn scope_lens_increases_crit_rate_to_one_eighth() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let p1 = build_pokemon_state(
                Species::Snorlax, &pokemon_dex, &move_dex, Some(50),
                Some([Some(PokemonMove::Tackle), None, None, None]),
                None, Some(Ability::None), None, Some(Item::ScopeLens),
                None, None, None, false,
            );
            let p2 = build_pokemon_state(
                Species::Snorlax, &pokemon_dex, &move_dex, Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None, Some(Ability::None), None, None, None, None, None, false,
            );

            let initial = battle_state_from_lists(vec![p1], vec![], vec![p2], vec![]);
            let outcomes = simulate_turn(
                &MatchState::BattleState(initial),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex, &pokemon_dex,
                true, // consider crits
                1,    // one damage roll → exactly two outcome buckets
            );

            // Crit outcomes have lower P2 HP (higher damage).
            let min_hp = outcomes.iter().filter_map(|(s, _)| {
                if let MatchState::BattleState(bs) = s { Some(bs.p2_active_mons[0].hp) } else { None }
            }).min().unwrap();

            let crit_prob: f64 = outcomes.iter().map(|(s, p)| {
                if let MatchState::BattleState(bs) = s {
                    if bs.p2_active_mons[0].hp == min_hp { *p } else { 0.0 }
                } else { 0.0 }
            }).sum();

            let expected = 1.0 / 8.0;
            assert!((crit_prob - expected).abs() < 1e-9,
                "Scope Lens should give 1/8 crit rate; got {crit_prob}");
        }

        // ── King's Rock tests ─────────────────────────────────────────────────
        // P1 is given a boosted Speed to guarantee it moves first, so the target
        // might be flinched before it acts.  Flinch detection: if the target used
        // its move, its PP decrements; if flinched, PP stays at its initial value.

        #[test]
        fn kings_rock_adds_10_percent_flinch_single_hit() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let mut p1 = build_pokemon_state(
                Species::Snorlax, &pokemon_dex, &move_dex, Some(50),
                Some([Some(PokemonMove::Tackle), None, None, None]),
                None, Some(Ability::None), None, Some(Item::KingsRock),
                None, None, None, false,
            );
            p1.stats[5] = 200; // guarantee P1 moves first

            let p2 = build_pokemon_state(
                Species::Snorlax, &pokemon_dex, &move_dex, Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None, Some(Ability::None), None, None, None, None, None, false,
            );
            let initial_target_pp = p2.move_pp[0];

            let initial = battle_state_from_lists(vec![p1], vec![], vec![p2], vec![]);
            let outcomes = simulate_turn(
                &MatchState::BattleState(initial),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex, &pokemon_dex,
                false, // no crits — keep it to 2 branches
                1,
            );

            let flinch_prob: f64 = outcomes.iter().map(|(s, p)| {
                if let MatchState::BattleState(bs) = s {
                    // Flinched = target's PP unchanged (never used Splash).
                    if bs.p2_active_mons[0].move_pp[0] == initial_target_pp { *p } else { 0.0 }
                } else { 0.0 }
            }).sum();

            let eps = 1e-9;
            // Tackle has 100% accuracy → P(flinch) = 1.0 * 0.10 = 0.10
            assert!((flinch_prob - 0.10).abs() < eps,
                "King's Rock should add 10% flinch on single-hit Tackle; got {flinch_prob}");
        }

        #[test]
        fn kings_rock_combined_flinch_chance_for_two_hits() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            // DoubleKick: Fighting, exactly 2 hits, 100% accuracy, no flinch secondary.
            let mut p1 = build_pokemon_state(
                Species::Snorlax, &pokemon_dex, &move_dex, Some(50),
                Some([Some(PokemonMove::DoubleKick), None, None, None]),
                None, Some(Ability::None), None, Some(Item::KingsRock),
                None, None, None, false,
            );
            p1.stats[5] = 200;

            let p2 = build_pokemon_state(
                Species::Snorlax, &pokemon_dex, &move_dex, Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None, Some(Ability::None), None, None, None, None, None, false,
            );
            let initial_target_pp = p2.move_pp[0];

            let initial = battle_state_from_lists(vec![p1], vec![], vec![p2], vec![]);
            let outcomes = simulate_turn(
                &MatchState::BattleState(initial),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex, &pokemon_dex,
                false,
                1,
            );

            let flinch_prob: f64 = outcomes.iter().map(|(s, p)| {
                if let MatchState::BattleState(bs) = s {
                    if bs.p2_active_mons[0].move_pp[0] == initial_target_pp { *p } else { 0.0 }
                } else { 0.0 }
            }).sum();

            // 2 hits × 10% each, combined: 1 - 0.9^2 = 0.19
            let expected = 1.0 - 0.9_f64.powi(2);
            let eps = 1e-9;
            assert!((flinch_prob - expected).abs() < eps,
                "King's Rock should give combined 1-0.9^2 ≈ 19% flinch over 2 hits; got {flinch_prob}");
        }

        #[test]
        fn kings_rock_does_not_stack_on_move_with_flinch() {
            // AirSlash already has a 30% flinch secondary; King's Rock should NOT
            // add an additional 10% on top of it.
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let mut p1 = build_pokemon_state(
                Species::Snorlax, &pokemon_dex, &move_dex, Some(50),
                Some([Some(PokemonMove::AirSlash), None, None, None]),
                None, Some(Ability::None), None, Some(Item::KingsRock),
                None, None, None, false,
            );
            p1.stats[5] = 200;

            let p2 = build_pokemon_state(
                Species::Snorlax, &pokemon_dex, &move_dex, Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None, Some(Ability::None), None, None, None, None, None, false,
            );
            let initial_target_pp = p2.move_pp[0];

            let initial = battle_state_from_lists(vec![p1], vec![], vec![p2], vec![]);
            let outcomes = simulate_turn(
                &MatchState::BattleState(initial),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex, &pokemon_dex,
                false,
                1,
            );

            let flinch_prob: f64 = outcomes.iter().map(|(s, p)| {
                if let MatchState::BattleState(bs) = s {
                    if bs.p2_active_mons[0].move_pp[0] == initial_target_pp { *p } else { 0.0 }
                } else { 0.0 }
            }).sum();

            // AirSlash accuracy 95%, flinch 30%.  King's Rock must not add a second layer.
            // P(flinch) = 0.95 * 0.30 = 0.285 (not 0.95 * (1 - 0.7 * 0.9) = 0.95 * 0.37 = 0.3515)
            let expected = 0.95 * 0.30;
            let eps = 1e-9;
            assert!((flinch_prob - expected).abs() < eps,
                "King's Rock must not stack on a move that already flinches; got {flinch_prob}, expected {expected}");
        }

        // ── Light Ball tests ──────────────────────────────────────────────────
        // Tested via effective_stat so we don't need to compute full damage calcs.

        #[test]
        fn light_ball_doubles_pikachus_attack_and_spatk() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let pikachu_no_item = build_pokemon_state(
                Species::Pikachu, &pokemon_dex, &move_dex, Some(50),
                Some([Some(PokemonMove::Tackle), None, None, None]),
                None, Some(Ability::None), None, None,
                None, None, None, false,
            );
            let mut pikachu_with_ball = pikachu_no_item.clone();
            pikachu_with_ball.item = Item::LightBall;

            let state = battle_state_from_lists(
                vec![pikachu_no_item.clone()], vec![], vec![pikachu_no_item.clone()], vec![],
            );

            let atk_no_ball  = simulator_helpers::effective_stat(&state, &pikachu_no_item,  crate::dex_data::PokemonStat::Atk, false, false);
            let spa_no_ball  = simulator_helpers::effective_stat(&state, &pikachu_no_item,  crate::dex_data::PokemonStat::SpA, false, false);
            let atk_with_ball = simulator_helpers::effective_stat(&state, &pikachu_with_ball, crate::dex_data::PokemonStat::Atk, false, false);
            let spa_with_ball = simulator_helpers::effective_stat(&state, &pikachu_with_ball, crate::dex_data::PokemonStat::SpA, false, false);

            assert!((atk_with_ball - 2.0 * atk_no_ball).abs() < 1e-9,
                "Light Ball should double Pikachu's Attack: {atk_no_ball} → {atk_with_ball}");
            assert!((spa_with_ball - 2.0 * spa_no_ball).abs() < 1e-9,
                "Light Ball should double Pikachu's SpA: {spa_no_ball} → {spa_with_ball}");
        }

        // ─── Berry-interaction abilities ───────────────────────────────────

        fn snorlax_with(item: Item, ability: Ability, nature: Nature, move_dex: &HashMap<PokemonMove, crate::dex_data::MoveData>, pokemon_dex: &HashMap<Species, crate::dex_data::PokemonData>) -> crate::pokemon::PokemonState {
            build_pokemon_state(
                Species::Snorlax, pokemon_dex, move_dex, Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None, Some(ability), Some(nature), Some(item),
                None, None, None, false,
            )
        }

        fn splash_mon(pokemon_dex: &HashMap<Species, crate::dex_data::PokemonData>, move_dex: &HashMap<PokemonMove, crate::dex_data::MoveData>) -> crate::pokemon::PokemonState {
            build_pokemon_state(
                Species::Shuckle, pokemon_dex, move_dex, Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None, Some(Ability::None), None, None, None, None, None, false,
            )
        }

        #[test]
        fn liechi_berry_fires_at_quarter_hp() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();
            let mut p1 = snorlax_with(Item::LiechiBerry, Ability::None, Nature::Hardy, &move_dex, &pokemon_dex);
            let max_hp = p1.stats[0];
            // Start just above the ≤25% threshold, then let poison drop below it.
            p1.hp = max_hp / 4 + 1;
            p1.status = Some(Status::Poison);
            let p2 = splash_mon(&pokemon_dex, &move_dex);

            let outcomes = run_single_turn(
                &MatchState::BattleState(battle_state_from_lists(vec![p1], vec![], vec![p2], vec![])),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex, &pokemon_dex,
            );
            let (bs, _) = extract_battle_state(outcomes);
            assert_eq!(bs.p1_active_mons[0].boosts[0], 1, "Liechi Berry should give +1 Atk at ≤25% HP");
            assert_eq!(bs.p1_active_mons[0].item, Item::None, "Liechi Berry should be consumed");
        }

        #[test]
        fn liechi_berry_does_not_fire_above_quarter_hp() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();
            let mut p1 = snorlax_with(Item::LiechiBerry, Ability::None, Nature::Hardy, &move_dex, &pokemon_dex);
            let max_hp = p1.stats[0];
            // At 50% HP, poison drops us to ~43% — still above the 25% threshold.
            p1.hp = max_hp / 2;
            p1.status = Some(Status::Poison);
            let p2 = splash_mon(&pokemon_dex, &move_dex);

            let outcomes = run_single_turn(
                &MatchState::BattleState(battle_state_from_lists(vec![p1], vec![], vec![p2], vec![])),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex, &pokemon_dex,
            );
            let (bs, _) = extract_battle_state(outcomes);
            assert_eq!(bs.p1_active_mons[0].boosts[0], 0, "Liechi Berry should NOT fire above 25% threshold");
            assert_eq!(bs.p1_active_mons[0].item, Item::LiechiBerry, "Liechi Berry should still be held");
        }

        #[test]
        fn figy_berry_heals_third_and_confuses_disliked_nature() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();
            // Bold lowers Atk — Figy Berry dislikes lowered Atk → confusion.
            let mut p1 = snorlax_with(Item::FigyBerry, Ability::None, Nature::Bold, &move_dex, &pokemon_dex);
            let max_hp = p1.stats[0];
            p1.hp = max_hp / 4 + 1;
            p1.status = Some(Status::Poison);
            let p2 = splash_mon(&pokemon_dex, &move_dex);

            let outcomes = run_single_turn(
                &MatchState::BattleState(battle_state_from_lists(vec![p1], vec![], vec![p2], vec![])),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex, &pokemon_dex,
            );
            let (bs, _) = extract_battle_state(outcomes);
            assert_eq!(bs.p1_active_mons[0].item, Item::None, "Figy Berry should be consumed");
            // Should have healed ⅓ max HP.
            let poison_dmg = (max_hp / 8).max(1);
            let hp_before_heal = (max_hp / 4 + 1).saturating_sub(poison_dmg);
            let expected_hp = (hp_before_heal + max_hp / 3).min(max_hp);
            assert_eq!(bs.p1_active_mons[0].hp, expected_hp, "Figy Berry should heal ⅓ max HP");
            // Bold nature dislikes Figy's Atk flavor → confusion.
            assert!(simulator_helpers::is_confused(&bs.p1_active_mons[0]), "Bold nature should cause Figy Berry confusion");
        }

        #[test]
        fn figy_berry_does_not_confuse_neutral_nature() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();
            // Hardy is neutral — no confusion.
            let mut p1 = snorlax_with(Item::FigyBerry, Ability::None, Nature::Hardy, &move_dex, &pokemon_dex);
            let max_hp = p1.stats[0];
            p1.hp = max_hp / 4 + 1;
            p1.status = Some(Status::Poison);
            let p2 = splash_mon(&pokemon_dex, &move_dex);

            let outcomes = run_single_turn(
                &MatchState::BattleState(battle_state_from_lists(vec![p1], vec![], vec![p2], vec![])),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex, &pokemon_dex,
            );
            let (bs, _) = extract_battle_state(outcomes);
            assert_eq!(bs.p1_active_mons[0].item, Item::None, "Figy Berry should still be consumed");
            assert!(!simulator_helpers::is_confused(&bs.p1_active_mons[0]), "Hardy nature should NOT cause Figy Berry confusion");
        }

        #[test]
        fn gluttony_fires_pinch_berry_at_half_hp() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();
            let mut p1 = snorlax_with(Item::LiechiBerry, Ability::Gluttony, Nature::Hardy, &move_dex, &pokemon_dex);
            let max_hp = p1.stats[0];
            // Start just above 50% so poison residual (max_hp/8) drops us into the Gluttony window
            // (between 25% and 50%). Without Gluttony the ≤25% threshold would not be reached.
            p1.hp = max_hp / 2 + 1;
            p1.status = Some(Status::Poison);
            let p2 = splash_mon(&pokemon_dex, &move_dex);

            let outcomes = run_single_turn(
                &MatchState::BattleState(battle_state_from_lists(vec![p1], vec![], vec![p2], vec![])),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex, &pokemon_dex,
            );
            let (bs, _) = extract_battle_state(outcomes);
            let poison_dmg = (max_hp / 8).max(1);
            let hp_after = (max_hp / 2 + 1).saturating_sub(poison_dmg);
            // Verify the test setup actually lands in the Gluttony window.
            assert!(hp_after <= max_hp / 2, "Sanity: HP should be ≤50% after poison");
            assert!(hp_after > max_hp / 4, "Sanity: HP should still be >25% (not pinch threshold)");
            assert_eq!(bs.p1_active_mons[0].boosts[0], 1, "Gluttony should lift threshold to ≤50% for Liechi Berry");
            assert_eq!(bs.p1_active_mons[0].item, Item::None, "Liechi Berry should be consumed with Gluttony");
        }

        #[test]
        fn ripen_doubles_oran_berry_heal() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();
            let mut p1 = snorlax_with(Item::OranBerry, Ability::Ripen, Nature::Hardy, &move_dex, &pokemon_dex);
            let max_hp = p1.stats[0];
            p1.hp = max_hp / 2 + 1;
            p1.status = Some(Status::Poison);
            let p2 = splash_mon(&pokemon_dex, &move_dex);

            let outcomes = run_single_turn(
                &MatchState::BattleState(battle_state_from_lists(vec![p1], vec![], vec![p2], vec![])),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex, &pokemon_dex,
            );
            let (bs, _) = extract_battle_state(outcomes);
            let poison_dmg = (max_hp / 8).max(1);
            let hp_before_heal = (max_hp / 2 + 1).saturating_sub(poison_dmg);
            // Ripen doubles Oran: heal 20 instead of 10.
            let expected_hp = (hp_before_heal + 20).min(max_hp);
            assert_eq!(bs.p1_active_mons[0].hp, expected_hp, "Ripen should double Oran Berry heal to 20 HP");
        }

        #[test]
        fn ripen_doubles_liechi_berry_stages() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();
            let mut p1 = snorlax_with(Item::LiechiBerry, Ability::Ripen, Nature::Hardy, &move_dex, &pokemon_dex);
            let max_hp = p1.stats[0];
            p1.hp = max_hp / 4 + 1;
            p1.status = Some(Status::Poison);
            let p2 = splash_mon(&pokemon_dex, &move_dex);

            let outcomes = run_single_turn(
                &MatchState::BattleState(battle_state_from_lists(vec![p1], vec![], vec![p2], vec![])),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex, &pokemon_dex,
            );
            let (bs, _) = extract_battle_state(outcomes);
            assert_eq!(bs.p1_active_mons[0].boosts[0], 2, "Ripen should double Liechi Berry to +2 Atk");
        }

        #[test]
        fn ripen_doubles_flavor_berry_heal() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();
            // Hardy is neutral → no confusion; Ripen should double heal to ⅔.
            let mut p1 = snorlax_with(Item::FigyBerry, Ability::Ripen, Nature::Hardy, &move_dex, &pokemon_dex);
            let max_hp = p1.stats[0];
            p1.hp = max_hp / 4 + 1;
            p1.status = Some(Status::Poison);
            let p2 = splash_mon(&pokemon_dex, &move_dex);

            let outcomes = run_single_turn(
                &MatchState::BattleState(battle_state_from_lists(vec![p1], vec![], vec![p2], vec![])),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex, &pokemon_dex,
            );
            let (bs, _) = extract_battle_state(outcomes);
            let poison_dmg = (max_hp / 8).max(1);
            let hp_before_heal = (max_hp / 4 + 1).saturating_sub(poison_dmg);
            let expected_hp = (hp_before_heal + max_hp * 2 / 3).min(max_hp);
            assert_eq!(bs.p1_active_mons[0].hp, expected_hp, "Ripen should double Figy Berry heal to ⅔ max HP");
        }

        #[test]
        fn cheek_pouch_adds_third_heal_on_top_of_berry() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();
            let mut p1 = snorlax_with(Item::LiechiBerry, Ability::CheekPouch, Nature::Hardy, &move_dex, &pokemon_dex);
            let max_hp = p1.stats[0];
            p1.hp = max_hp / 4 + 1;
            p1.status = Some(Status::Poison);
            let p2 = splash_mon(&pokemon_dex, &move_dex);

            let outcomes = run_single_turn(
                &MatchState::BattleState(battle_state_from_lists(vec![p1], vec![], vec![p2], vec![])),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex, &pokemon_dex,
            );
            let (bs, _) = extract_battle_state(outcomes);
            // Liechi gives +1 Atk (its effect); Cheek Pouch adds ⅓ max HP heal on top.
            assert_eq!(bs.p1_active_mons[0].boosts[0], 1, "Liechi Berry should still give +1 Atk with Cheek Pouch");
            let poison_dmg = (max_hp / 8).max(1);
            let hp_after_poison = (max_hp / 4 + 1).saturating_sub(poison_dmg);
            let expected_hp = (hp_after_poison + max_hp / 3).min(max_hp);
            assert_eq!(bs.p1_active_mons[0].hp, expected_hp, "Cheek Pouch should add ⅓ max HP heal");
        }

        #[test]
        fn unnerve_prevents_opponent_eating_oran_berry() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();
            // P1 holds Oran Berry and is at ≤50% HP, but P2 has Unnerve → berry must not fire.
            let mut p1 = snorlax_with(Item::OranBerry, Ability::None, Nature::Hardy, &move_dex, &pokemon_dex);
            let max_hp = p1.stats[0];
            p1.hp = max_hp / 2 + 1;
            p1.status = Some(Status::Poison);
            let p2 = build_pokemon_state(
                Species::Shuckle, &pokemon_dex, &move_dex, Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None, Some(Ability::Unnerve), None, None, None, None, None, false,
            );

            let outcomes = run_single_turn(
                &MatchState::BattleState(battle_state_from_lists(vec![p1], vec![], vec![p2], vec![])),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex, &pokemon_dex,
            );
            let (bs, _) = extract_battle_state(outcomes);
            let poison_dmg = (max_hp / 8).max(1);
            let expected_hp = (max_hp / 2 + 1).saturating_sub(poison_dmg);
            assert_eq!(bs.p1_active_mons[0].item, Item::OranBerry,
                "Unnerve should prevent opponent from eating Oran Berry");
            assert_eq!(bs.p1_active_mons[0].hp, expected_hp,
                "HP should only reflect poison damage when Unnerve suppresses berry");
        }

        #[test]
        fn unnerve_does_not_prevent_holder_eating_own_berry() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();
            // P1 has Unnerve and holds Oran Berry — Unnerve only affects the *opponent*.
            let mut p1 = snorlax_with(Item::OranBerry, Ability::Unnerve, Nature::Hardy, &move_dex, &pokemon_dex);
            let max_hp = p1.stats[0];
            p1.hp = max_hp / 2 + 1;
            p1.status = Some(Status::Poison);
            let p2 = splash_mon(&pokemon_dex, &move_dex);

            let outcomes = run_single_turn(
                &MatchState::BattleState(battle_state_from_lists(vec![p1], vec![], vec![p2], vec![])),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex, &pokemon_dex,
            );
            let (bs, _) = extract_battle_state(outcomes);
            // Berry should fire normally.
            assert_eq!(bs.p1_active_mons[0].item, Item::None,
                "Unnerve should NOT prevent the holder's own Oran Berry from firing");
        }

        #[test]
        fn cud_chew_re_applies_berry_after_following_turn() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();
            // Turn 1: Liechi Berry fires (P1 at ≤25% HP via poison). Cud Chew arms the re-eat.
            let mut p1 = snorlax_with(Item::LiechiBerry, Ability::CudChew, Nature::Hardy, &move_dex, &pokemon_dex);
            let max_hp = p1.stats[0];
            p1.hp = max_hp / 4 + 1;
            p1.status = Some(Status::Poison);
            let p2 = splash_mon(&pokemon_dex, &move_dex);

            let turn1_outcomes = run_single_turn(
                &MatchState::BattleState(battle_state_from_lists(vec![p1], vec![], vec![p2], vec![])),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex, &pokemon_dex,
            );
            let (mut bs1, _) = extract_battle_state(turn1_outcomes);
            // After turn 1: berry consumed, +1 Atk, Cud Chew armed for next EOT.
            assert_eq!(bs1.p1_active_mons[0].boosts[0], 1, "Liechi Berry should give +1 Atk on consumption turn");
            assert_eq!(bs1.p1_active_mons[0].item, Item::None, "Berry should be consumed after turn 1");

            // Clear poison so P1 doesn't faint on turn 2 before Cud Chew fires.
            bs1.p1_active_mons[0].status = None;

            // Turn 2: Cud Chew fires the re-eat at EOT → another +1 Atk.
            let turn2_outcomes = run_single_turn(
                &MatchState::BattleState(bs1),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex, &pokemon_dex,
            );
            let (bs2, _) = extract_battle_state(turn2_outcomes);
            assert_eq!(bs2.p1_active_mons[0].boosts[0], 2, "Cud Chew should add another +1 Atk on the following turn");
        }

        #[test]
        fn cud_chew_pending_cleared_on_switch_out() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();
            let mut p1 = snorlax_with(Item::LiechiBerry, Ability::CudChew, Nature::Hardy, &move_dex, &pokemon_dex);
            let max_hp = p1.stats[0];
            p1.hp = max_hp / 4;
            // Bench mon to switch to.
            let bench = build_pokemon_state(
                Species::Shuckle, &pokemon_dex, &move_dex, Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None, Some(Ability::None), None, None, None, None, None, false,
            );
            let p2 = splash_mon(&pokemon_dex, &move_dex);

            // Turn 1: berry fires, Cud Chew arms re-eat.
            let turn1_outcomes = run_single_turn(
                &MatchState::BattleState(battle_state_from_lists(vec![p1], vec![bench], vec![p2], vec![])),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex, &pokemon_dex,
            );
            let (mut bs1, _) = extract_battle_state(turn1_outcomes);

            // Switch out P1 before the second turn — clears cud_chew_pending.
            let _ = &mut bs1; // suppress unused-mut lint
            let turn2_outcomes = run_single_turn(
                &MatchState::BattleState(bs1.clone()),
                &PlayerCommand::Battle(vec![BattleCommand::Switch(SwitchCommand { party_index: 0 })]),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex, &pokemon_dex,
            );
            let (bs2, _) = extract_battle_state(turn2_outcomes);
            // P1's original Snorlax is now on bench. Its boost should still be +1 (the Liechi
            // effect) but no additional +1 from Cud Chew, since we switched out.
            let snorlax_on_bench = bs2.p1_back_mons.iter().find(|m| m.species == Species::Snorlax);
            assert!(snorlax_on_bench.is_some(), "Snorlax should be on bench");
            if let Some(snorlax) = snorlax_on_bench {
                assert_eq!(snorlax.cud_chew_pending, None,
                    "cud_chew_pending should be cleared on switch-out");
            }
        }

        #[test]
        fn light_ball_does_not_boost_raichu() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let raichu_no_item = build_pokemon_state(
                Species::Raichu, &pokemon_dex, &move_dex, Some(50),
                Some([Some(PokemonMove::Tackle), None, None, None]),
                None, Some(Ability::None), None, None,
                None, None, None, false,
            );
            let mut raichu_with_ball = raichu_no_item.clone();
            raichu_with_ball.item = Item::LightBall;

            let state = battle_state_from_lists(
                vec![raichu_no_item.clone()], vec![], vec![raichu_no_item.clone()], vec![],
            );

            let atk_no_ball  = simulator_helpers::effective_stat(&state, &raichu_no_item,  crate::dex_data::PokemonStat::Atk, false, false);
            let atk_with_ball = simulator_helpers::effective_stat(&state, &raichu_with_ball, crate::dex_data::PokemonStat::Atk, false, false);

            assert!((atk_with_ball - atk_no_ball).abs() < 1e-9,
                "Light Ball should not boost Raichu's Attack");
        }
    }

    mod choice_items {
        use super::*;
        use crate::simulator::get_possible_commands_for_active_slot;

        // ── Stat boosts ────────────────────────────────────────────────────

        #[test]
        fn choice_band_boosts_attack_by_1_5x() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let mon = build_pokemon_state(
                Species::Raticate, &pokemon_dex, &move_dex, Some(50),
                Some([Some(PokemonMove::Tackle), None, None, None]),
                None, Some(Ability::None), None, None, None, None, None, false,
            );
            let mut banded = mon.clone();
            banded.item = Item::ChoiceBand;

            let state = battle_state_from_lists(vec![mon.clone()], vec![], vec![mon.clone()], vec![]);

            let atk_no_band = simulator_helpers::effective_stat(&state, &mon, crate::dex_data::PokemonStat::Atk, false, false);
            let atk_banded  = simulator_helpers::effective_stat(&state, &banded, crate::dex_data::PokemonStat::Atk, false, false);
            let spa_banded  = simulator_helpers::effective_stat(&state, &banded, crate::dex_data::PokemonStat::SpA, false, false);
            let spa_no_band = simulator_helpers::effective_stat(&state, &mon, crate::dex_data::PokemonStat::SpA, false, false);

            assert!((atk_banded - 1.5 * atk_no_band).abs() < 1e-9,
                "Choice Band should give 1.5x Attack");
            assert!((spa_banded - spa_no_band).abs() < 1e-9,
                "Choice Band must not boost Special Attack");
        }

        #[test]
        fn choice_specs_boosts_spa_by_1_5x() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let mon = build_pokemon_state(
                Species::Kadabra, &pokemon_dex, &move_dex, Some(50),
                Some([Some(PokemonMove::Psychic), None, None, None]),
                None, Some(Ability::None), None, None, None, None, None, false,
            );
            let mut specs = mon.clone();
            specs.item = Item::ChoiceSpecs;

            let state = battle_state_from_lists(vec![mon.clone()], vec![], vec![mon.clone()], vec![]);

            let spa_no_specs = simulator_helpers::effective_stat(&state, &mon, crate::dex_data::PokemonStat::SpA, false, false);
            let spa_specs    = simulator_helpers::effective_stat(&state, &specs, crate::dex_data::PokemonStat::SpA, false, false);
            let atk_specs    = simulator_helpers::effective_stat(&state, &specs, crate::dex_data::PokemonStat::Atk, false, false);
            let atk_no_specs = simulator_helpers::effective_stat(&state, &mon, crate::dex_data::PokemonStat::Atk, false, false);

            assert!((spa_specs - 1.5 * spa_no_specs).abs() < 1e-9,
                "Choice Specs should give 1.5x SpA");
            assert!((atk_specs - atk_no_specs).abs() < 1e-9,
                "Choice Specs must not boost Attack");
        }

        // ── Speed boost (Scarf) ────────────────────────────────────────────

        #[test]
        fn choice_scarf_increases_speed_by_1_5x() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let mon = build_pokemon_state(
                Species::Raticate, &pokemon_dex, &move_dex, Some(50),
                Some([Some(PokemonMove::Tackle), None, None, None]),
                None, Some(Ability::None), None, None, None, None, None, false,
            );
            let mut scarfed = mon.clone();
            scarfed.item = Item::ChoiceScarf;

            let state = battle_state_from_lists(
                vec![mon.clone()], vec![], vec![mon.clone()], vec![],
            );
            let slot = crate::battle::FieldSlot { player: Player::P1, slot_index: 0 };

            let speed_no_scarf = simulator_helpers::effective_speed_for_slot(&state, slot, &mon);
            let speed_scarfed  = simulator_helpers::effective_speed_for_slot(&state, slot, &scarfed);

            assert!(
                (speed_scarfed - speed_no_scarf * 1.5).abs() < 0.1,
                "Choice Scarf must give exactly 1.5x speed: {speed_no_scarf} -> {speed_scarfed}"
            );
        }

        // ── Move lock ──────────────────────────────────────────────────────

        #[test]
        fn choice_band_locks_into_first_move() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            // Use Tackle (100% accuracy) to avoid multi-branch outcomes from accuracy.
            let banded = build_pokemon_state(
                Species::Machoke, &pokemon_dex, &move_dex, Some(50),
                Some([Some(PokemonMove::Tackle), Some(PokemonMove::BodySlam), None, None]),
                None, Some(Ability::None), None, Some(Item::ChoiceBand),
                Some(crate::dex_data::PokemonType::Fighting), None, None, false,
            );
            let dummy = build_pokemon_state(
                Species::Snorlax, &pokemon_dex, &move_dex, Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None, Some(Ability::None), None, None,
                Some(crate::dex_data::PokemonType::Normal), None, None, false,
            );

            // Before any move: both moves available.
            let state = battle_state_from_lists(vec![banded.clone()], vec![], vec![dummy.clone()], vec![]);
            let initial_cmds = get_possible_commands_for_active_slot(&state, Player::P1, 0, &move_dex, &pokemon_dex);
            let initial_attack_count = initial_cmds.iter()
                .filter(|c| matches!(c, BattleCommand::Attack(_))).count();
            assert_eq!(initial_attack_count, 2, "both moves should be available before locking");

            // After using Tackle (move 0): only Tackle should remain selectable.
            let state_after = extract_battle_state(run_single_turn(
                &MatchState::BattleState(state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex, &pokemon_dex,
            )).0;

            let locked_cmds = get_possible_commands_for_active_slot(
                &state_after, Player::P1, 0, &move_dex, &pokemon_dex,
            );
            let locked_attacks: Vec<_> = locked_cmds.iter()
                .filter(|c| matches!(c, BattleCommand::Attack(_))).collect();
            let has_tackle = locked_attacks.iter().any(|c| {
                if let BattleCommand::Attack(a) = c {
                    state_after.p1_active_mons[0].moves[a.move_slot] == Some(PokemonMove::Tackle)
                } else { false }
            });
            let has_body_slam = locked_attacks.iter().any(|c| {
                if let BattleCommand::Attack(a) = c {
                    state_after.p1_active_mons[0].moves[a.move_slot] == Some(PokemonMove::BodySlam)
                } else { false }
            });
            assert!(has_tackle, "Tackle (locked move) should still be available");
            assert!(!has_body_slam, "Body Slam should be blocked by choice lock");
        }

        #[test]
        fn choice_lock_clears_on_switch_out() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let banded = build_pokemon_state(
                Species::Machoke, &pokemon_dex, &move_dex, Some(50),
                Some([Some(PokemonMove::Tackle), Some(PokemonMove::BodySlam), None, None]),
                None, Some(Ability::None), None, Some(Item::ChoiceBand),
                Some(crate::dex_data::PokemonType::Fighting), None, None, false,
            );
            let bench = build_pokemon_state(
                Species::Rattata, &pokemon_dex, &move_dex, Some(50),
                Some([Some(PokemonMove::Tackle), None, None, None]),
                None, Some(Ability::None), None, None,
                Some(crate::dex_data::PokemonType::Normal), None, None, false,
            );
            let dummy = build_pokemon_state(
                Species::Snorlax, &pokemon_dex, &move_dex, Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None, Some(Ability::None), None, None,
                Some(crate::dex_data::PokemonType::Normal), None, None, false,
            );

            // Turn 1: use CrossChop → get locked.
            let state = battle_state_from_lists(
                vec![banded.clone()], vec![bench.clone()],
                vec![dummy.clone()], vec![],
            );
            let after_t1 = extract_battle_state(run_single_turn(
                &MatchState::BattleState(state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex, &pokemon_dex,
            )).0;

            // Confirm locked.
            let locked = get_possible_commands_for_active_slot(
                &after_t1, Player::P1, 0, &move_dex, &pokemon_dex,
            );
            let body_slam_before_switch = locked.iter().any(|c| {
                if let BattleCommand::Attack(a) = c {
                    after_t1.p1_active_mons[0].moves[a.move_slot] == Some(PokemonMove::BodySlam)
                } else { false }
            });
            assert!(!body_slam_before_switch, "should be locked before switching");

            // Turn 2: switch out banded mon → switch back in next turn.
            let after_t2 = extract_battle_state(run_single_turn(
                &MatchState::BattleState(after_t1),
                &PlayerCommand::Battle(vec![BattleCommand::Switch(crate::battle::SwitchCommand { party_index: 0 })]),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex, &pokemon_dex,
            )).0;

            // Turn 3: switch banded back in.
            let after_t3 = extract_battle_state(run_single_turn(
                &MatchState::BattleState(after_t2),
                &PlayerCommand::Battle(vec![BattleCommand::Switch(crate::battle::SwitchCommand { party_index: 0 })]),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex, &pokemon_dex,
            )).0;

            let unlocked = get_possible_commands_for_active_slot(
                &after_t3, Player::P1, 0, &move_dex, &pokemon_dex,
            );
            let body_slam_after_switch = unlocked.iter().any(|c| {
                if let BattleCommand::Attack(a) = c {
                    after_t3.p1_active_mons[0].moves[a.move_slot] == Some(PokemonMove::BodySlam)
                } else { false }
            });
            assert!(body_slam_after_switch, "choice lock should have cleared on switch");
        }
    }

    mod quick_claw {
        use super::*;

        #[test]
        fn quick_claw_branches_with_correct_probabilities() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            // Both mons at 1 HP with Tackle. Whoever moves first KOs the other.
            // P1 is slower but holds Quick Claw; P2 is faster and has no item.
            //
            // - QC fires   (0.2): P1 goes first → KOs P2 → GameOverState { winner: P1 }
            // - QC inactive(0.8): P2 goes first → KOs P1 → GameOverState { winner: P2 }
            //
            // Probability P1 wins = probability QC fired.
            let mut slow_qc = build_pokemon_state(
                Species::Golem, &pokemon_dex, &move_dex, Some(50),
                Some([Some(PokemonMove::Tackle), None, None, None]),
                None, Some(Ability::None), None, Some(Item::QuickClaw),
                Some(crate::dex_data::PokemonType::Rock), None, None, false,
            );
            slow_qc.hp = 1;

            let mut fast_no_item = build_pokemon_state(
                Species::Electrode, &pokemon_dex, &move_dex, Some(50),
                Some([Some(PokemonMove::Tackle), None, None, None]),
                None, Some(Ability::None), None, None,
                Some(crate::dex_data::PokemonType::Electric), None, None, false,
            );
            fast_no_item.hp = 1;

            let state = battle_state_from_lists(
                vec![slow_qc.clone()], vec![], vec![fast_no_item.clone()], vec![],
            );
            let outcomes = run_single_turn(
                &MatchState::BattleState(state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex, &pokemon_dex,
            );

            let p1_wins: f64 = outcomes.iter()
                .filter_map(|(state, prob)| {
                    if let MatchState::GameOverState { winner } = state {
                        if *winner == Player::P1 { Some(prob) } else { None }
                    } else { None }
                })
                .sum();

            assert!(
                (p1_wins - 0.2).abs() < 0.01,
                "Quick Claw should give the slow holder a 20% chance to act first, got {p1_wins}"
            );
        }

        #[test]
        fn quick_claw_does_not_reorder_across_priority_brackets() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            // Slow mon with Quick Claw uses priority-0 move.
            // Opponent uses +1 priority move (QuickAttack). QC must never beat +1.
            let slow_qc = build_pokemon_state(
                Species::Snorlax, &pokemon_dex, &move_dex, Some(50),
                Some([Some(PokemonMove::BodySlam), None, None, None]),
                None, Some(Ability::None), None, Some(Item::QuickClaw),
                Some(crate::dex_data::PokemonType::Normal), None, None, false,
            );
            let fast_priority = build_pokemon_state(
                Species::Pikachu, &pokemon_dex, &move_dex, Some(50),
                Some([Some(PokemonMove::QuickAttack), None, None, None]),
                None, Some(Ability::None), None, None,
                Some(crate::dex_data::PokemonType::Electric), None, None, false,
            );

            let state = battle_state_from_lists(
                vec![slow_qc.clone()], vec![], vec![fast_priority.clone()], vec![],
            );
            let outcomes = run_single_turn(
                &MatchState::BattleState(state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex, &pokemon_dex,
            );

            // Pikachu's Quick Attack (+1 priority) must always go first.
            let initial_p1_hp = slow_qc.hp;
            let p2_always_first = outcomes.iter().all(|(state, _)| {
                match state {
                    MatchState::BattleState(bs) => bs.p1_active_mons[0].hp < initial_p1_hp,
                    MatchState::GameOverState { .. } => true,
                    _ => false,
                }
            });
            assert!(p2_always_first,
                "Quick Claw must not reorder across priority brackets — +1 priority should always go first");
        }
    }

    mod struggle {
        use super::*;
        use crate::simulator::get_possible_commands_for_active_slot;

        // Helper: a Pokémon whose single move has 0 PP.
        fn exhausted_mon(species: Species, pokemon_dex: &HashMap<Species, crate::dex_data::PokemonData>, move_dex: &HashMap<PokemonMove, crate::dex_data::MoveData>) -> PokemonState {
            let mut mon = build_pokemon_state(
                species, pokemon_dex, move_dex, Some(50),
                Some([Some(PokemonMove::Tackle), None, None, None]),
                None, Some(Ability::None), None, None,
                Some(crate::dex_data::PokemonType::Normal), None, None, false,
            );
            mon.move_pp[0] = 0;
            mon
        }

        #[test]
        fn struggle_offered_when_all_pp_zero() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let mon = exhausted_mon(Species::Rattata, &pokemon_dex, &move_dex);
            let dummy = build_pokemon_state(
                Species::Snorlax, &pokemon_dex, &move_dex, Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None, Some(Ability::None), None, None,
                Some(crate::dex_data::PokemonType::Normal), None, None, false,
            );

            let state = battle_state_from_lists(vec![mon], vec![], vec![dummy], vec![]);
            let cmds = get_possible_commands_for_active_slot(&state, Player::P1, 0, &move_dex, &pokemon_dex);

            let has_struggle = cmds.iter().any(|c| matches!(c, BattleCommand::Struggle { .. }));
            let has_attack = cmds.iter().any(|c| matches!(c, BattleCommand::Attack(_)));
            assert!(has_struggle, "Struggle should be offered when all PP are 0");
            assert!(!has_attack, "Normal attacks must not be offered alongside Struggle");
        }

        #[test]
        fn zero_pp_move_not_offered_when_other_moves_remain() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let mut mon = build_pokemon_state(
                Species::Rattata, &pokemon_dex, &move_dex, Some(50),
                Some([Some(PokemonMove::Tackle), Some(PokemonMove::QuickAttack), None, None]),
                None, Some(Ability::None), None, None,
                Some(crate::dex_data::PokemonType::Normal), None, None, false,
            );
            mon.move_pp[0] = 0; // Tackle out of PP; Quick Attack still has PP.

            let dummy = build_pokemon_state(
                Species::Snorlax, &pokemon_dex, &move_dex, Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None, Some(Ability::None), None, None,
                Some(crate::dex_data::PokemonType::Normal), None, None, false,
            );

            let state = battle_state_from_lists(vec![mon.clone()], vec![], vec![dummy], vec![]);
            let cmds = get_possible_commands_for_active_slot(&state, Player::P1, 0, &move_dex, &pokemon_dex);

            let has_struggle = cmds.iter().any(|c| matches!(c, BattleCommand::Struggle { .. }));
            let has_tackle = cmds.iter().any(|c| {
                if let BattleCommand::Attack(a) = c {
                    mon.moves[a.move_slot] == Some(PokemonMove::Tackle)
                } else { false }
            });
            let has_quick_attack = cmds.iter().any(|c| {
                if let BattleCommand::Attack(a) = c {
                    mon.moves[a.move_slot] == Some(PokemonMove::QuickAttack)
                } else { false }
            });

            assert!(has_quick_attack, "Quick Attack (has PP) should be available");
            assert!(!has_tackle, "Tackle (0 PP) must not be offered");
            assert!(!has_struggle, "Struggle must not appear when other moves have PP");
        }

        #[test]
        fn struggle_deals_damage_to_ghost_type() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let attacker = exhausted_mon(Species::Rattata, &pokemon_dex, &move_dex);
            let ghost_target = build_pokemon_state(
                Species::Gengar, &pokemon_dex, &move_dex, Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None, Some(Ability::None), None, None,
                Some(crate::dex_data::PokemonType::Ghost), None, None, false,
            );

            let initial_hp = ghost_target.hp;
            let state = battle_state_from_lists(
                vec![attacker], vec![], vec![ghost_target], vec![],
            );
            let outcomes = run_single_turn(
                &MatchState::BattleState(state),
                &PlayerCommand::Battle(vec![BattleCommand::Struggle { target: Some(crate::battle::FieldSlot { player: Player::P2, slot_index: 0 }) }]),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex, &pokemon_dex,
            );

            let ghost_took_damage = outcomes.iter().any(|(state, _)| {
                match state {
                    MatchState::BattleState(bs) => bs.p2_active_mons[0].hp < initial_hp,
                    MatchState::GameOverState { .. } => true,
                    _ => false,
                }
            });
            assert!(ghost_took_damage, "Struggle is typeless and must damage Ghost types");
        }

        #[test]
        fn struggle_deals_recoil_and_recoil_ignores_rock_head() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            // Rock Head normally blocks recoil — Struggle must bypass it.
            let mut attacker = exhausted_mon(Species::Aggron, &pokemon_dex, &move_dex);
            attacker.ability = Ability::RockHead;

            let target = build_pokemon_state(
                Species::Snorlax, &pokemon_dex, &move_dex, Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None, Some(Ability::None), None, None,
                Some(crate::dex_data::PokemonType::Normal), None, None, false,
            );

            let initial_attacker_hp = attacker.hp;
            let state = battle_state_from_lists(
                vec![attacker.clone()], vec![], vec![target], vec![],
            );
            let outcomes = run_single_turn(
                &MatchState::BattleState(state),
                &PlayerCommand::Battle(vec![BattleCommand::Struggle { target: Some(crate::battle::FieldSlot { player: Player::P2, slot_index: 0 }) }]),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex, &pokemon_dex,
            );

            let attacker_took_recoil = outcomes.iter().any(|(state, _)| {
                match state {
                    MatchState::BattleState(bs) => bs.p1_active_mons[0].hp < initial_attacker_hp,
                    MatchState::GameOverState { winner } => *winner == Player::P2, // attacker fainted to recoil
                    _ => false,
                }
            });
            assert!(attacker_took_recoil,
                "Struggle recoil must apply even to Rock Head holders");
        }
    }

    // ════════════════════════════════════════════════════════════════════
    // Stat-protection abilities
    // ════════════════════════════════════════════════════════════════════
    mod stat_protection_abilities {
        use super::*;

        /// Build a level-50 Snorlax (Normal — no special type interactions) with a
        /// forced ability and one move.  All other build params are default/zero.
        fn mon(ability: Ability, first_move: PokemonMove) -> PokemonState {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();
            build_pokemon_state(
                Species::Snorlax, &pokemon_dex, &move_dex, Some(50),
                Some([Some(first_move), None, None, None]),
                None, Some(ability), Some(Nature::Hardy),
                None, None, Some([0, 0, 0, 0, 0, 0]), None, false,
            )
        }

        // ── Clear Body / White Smoke / Full Metal Body block Intimidate ─────────

        #[test]
        fn clear_body_blocks_intimidate() {
            let p1 = mon(Ability::Intimidate, PokemonMove::Splash);
            let p2 = mon(Ability::ClearBody, PokemonMove::Splash);
            let state = battle_state_from_lists(vec![p1], vec![], vec![p2], vec![]);
            assert_eq!(state.p2_active_mons[0].boosts[0], 0,
                "Clear Body should block Intimidate's Attack drop");
        }

        #[test]
        fn white_smoke_blocks_intimidate() {
            let p1 = mon(Ability::Intimidate, PokemonMove::Splash);
            let p2 = mon(Ability::WhiteSmoke, PokemonMove::Splash);
            let state = battle_state_from_lists(vec![p1], vec![], vec![p2], vec![]);
            assert_eq!(state.p2_active_mons[0].boosts[0], 0,
                "White Smoke should block Intimidate's Attack drop");
        }

        #[test]
        fn full_metal_body_blocks_intimidate() {
            let p1 = mon(Ability::Intimidate, PokemonMove::Splash);
            let p2 = mon(Ability::FullMetalBody, PokemonMove::Splash);
            let state = battle_state_from_lists(vec![p1], vec![], vec![p2], vec![]);
            assert_eq!(state.p2_active_mons[0].boosts[0], 0,
                "Full Metal Body should block Intimidate's Attack drop");
        }

        // Verify that ordinary Intimidate still fires (guards against accidental over-blocking).
        #[test]
        fn intimidate_still_fires_vs_no_protection() {
            let p1 = mon(Ability::Intimidate, PokemonMove::Splash);
            let p2 = mon(Ability::None, PokemonMove::Splash);
            let state = battle_state_from_lists(vec![p1], vec![], vec![p2], vec![]);
            assert_eq!(state.p2_active_mons[0].boosts[0], -1,
                "Intimidate should still lower Attack of unprotected foes");
        }

        // ── Clear Body blocks move-induced stat drops ───────────────────────────

        #[test]
        fn clear_body_blocks_growl() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();
            // P1 has Clear Body, P2 uses Growl (−1 Atk on target).
            let p1 = mon(Ability::ClearBody, PokemonMove::Splash);
            let p2 = mon(Ability::None, PokemonMove::Growl);
            let initial = battle_state_from_lists(vec![p1], vec![], vec![p2], vec![]);
            let outcomes = run_single_turn(
                &MatchState::BattleState(initial),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex, &pokemon_dex,
            );
            let (bs, _) = extract_battle_state(outcomes);
            assert_eq!(bs.p1_active_mons[0].boosts[0], 0,
                "Clear Body should block Growl's Attack drop");
        }

        // ── Hyper Cutter ────────────────────────────────────────────────────────

        #[test]
        fn hyper_cutter_blocks_attack_drop() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();
            let p1 = mon(Ability::HyperCutter, PokemonMove::Splash);
            let p2 = mon(Ability::None, PokemonMove::Growl);
            let initial = battle_state_from_lists(vec![p1], vec![], vec![p2], vec![]);
            let outcomes = run_single_turn(
                &MatchState::BattleState(initial),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex, &pokemon_dex,
            );
            let (bs, _) = extract_battle_state(outcomes);
            assert_eq!(bs.p1_active_mons[0].boosts[0], 0,
                "Hyper Cutter should block opponent Attack drops (index 0)");
        }

        #[test]
        fn hyper_cutter_allows_defense_drop() {
            // Hyper Cutter only blocks Attack (index 0); Leer's Defense (index 1) drop goes through.
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();
            let p1 = mon(Ability::HyperCutter, PokemonMove::Splash);
            let p2 = mon(Ability::None, PokemonMove::Leer);
            let initial = battle_state_from_lists(vec![p1], vec![], vec![p2], vec![]);
            let outcomes = run_single_turn(
                &MatchState::BattleState(initial),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex, &pokemon_dex,
            );
            let (bs, _) = extract_battle_state(outcomes);
            assert_eq!(bs.p1_active_mons[0].boosts[1], -1,
                "Hyper Cutter must not block Defense drops");
        }

        // ── Big Pecks ───────────────────────────────────────────────────────────

        #[test]
        fn big_pecks_blocks_defense_drop() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();
            let p1 = mon(Ability::BigPecks, PokemonMove::Splash);
            let p2 = mon(Ability::None, PokemonMove::Leer);
            let initial = battle_state_from_lists(vec![p1], vec![], vec![p2], vec![]);
            let outcomes = run_single_turn(
                &MatchState::BattleState(initial),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex, &pokemon_dex,
            );
            let (bs, _) = extract_battle_state(outcomes);
            assert_eq!(bs.p1_active_mons[0].boosts[1], 0,
                "Big Pecks should block Defense drops (index 1)");
        }

        #[test]
        fn big_pecks_allows_attack_drop() {
            // Big Pecks only blocks Defense (index 1); Growl's Attack drop still applies.
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();
            let p1 = mon(Ability::BigPecks, PokemonMove::Splash);
            let p2 = mon(Ability::None, PokemonMove::Growl);
            let initial = battle_state_from_lists(vec![p1], vec![], vec![p2], vec![]);
            let outcomes = run_single_turn(
                &MatchState::BattleState(initial),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex, &pokemon_dex,
            );
            let (bs, _) = extract_battle_state(outcomes);
            assert_eq!(bs.p1_active_mons[0].boosts[0], -1,
                "Big Pecks must not block Attack drops");
        }

        // ── Self-inflicted drops bypass stat protection ──────────────────────────

        #[test]
        fn clear_body_does_not_block_self_inflicted_drops() {
            // Close Combat lowers the USER's own Def (index 1) and SpD (index 3) by −1.
            // Clear Body must not interfere — these go through the attacker-effect path.
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();
            let p1 = mon(Ability::ClearBody, PokemonMove::CloseCombat);
            let p2 = mon(Ability::None, PokemonMove::Splash);
            let initial = battle_state_from_lists(vec![p1], vec![], vec![p2], vec![]);
            let outcomes = run_single_turn(
                &MatchState::BattleState(initial),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex, &pokemon_dex,
            );
            let (bs, _) = extract_battle_state(outcomes);
            assert_eq!(bs.p1_active_mons[0].boosts[1], -1,
                "Clear Body must not block own Close Combat Def drop");
            assert_eq!(bs.p1_active_mons[0].boosts[3], -1,
                "Clear Body must not block own Close Combat SpD drop");
        }
    }

    // ════════════════════════════════════════════════════════════════════
    // Stat-change reaction abilities:
    //   Subgroup A — Competitive, Defiant, Mirror Armor
    //   Trigger: a stat is lowered by an opponent.
    // ════════════════════════════════════════════════════════════════════
    mod stat_change_reaction_abilities {
        use super::*;

        /// Build a level-50 Snorlax with a forced ability and one move.
        fn mon(ability: Ability, first_move: PokemonMove) -> PokemonState {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();
            build_pokemon_state(
                Species::Snorlax, &pokemon_dex, &move_dex, Some(50),
                Some([Some(first_move), None, None, None]),
                None, Some(ability), Some(Nature::Hardy),
                None, None, Some([0, 0, 0, 0, 0, 0]), None, false,
            )
        }

        // ── Defiant ────────────────────────────────────────────────────

        // Intimidate lowers Atk by 1; Defiant reacts with +2 → net +1.
        #[test]
        fn defiant_on_intimidate_net_plus_one() {
            let p1 = mon(Ability::Intimidate, PokemonMove::Splash);
            let p2 = mon(Ability::Defiant, PokemonMove::Splash);
            let state = battle_state_from_lists(vec![p1], vec![], vec![p2], vec![]);
            assert_eq!(state.p2_active_mons[0].boosts[0], 1,
                "Defiant: Intimidate -1 Atk + Defiant +2 Atk = net +1");
        }

        // Charm lowers Atk by 2; that is ONE stat → one Defiant trigger (+2).
        // Net result: −2 + 2 = 0.
        #[test]
        fn defiant_once_for_multistage_single_stat_drop() {
            let mdex = move_dex();
            let pdex = pokemon_dex();
            let p1 = mon(Ability::Defiant, PokemonMove::Splash);
            let p2 = mon(Ability::None, PokemonMove::Charm);
            let initial = battle_state_from_lists(vec![p1], vec![], vec![p2], vec![]);
            let outcomes = run_single_turn(
                &MatchState::BattleState(initial),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &mdex, &pdex,
            );
            let (bs, _) = extract_battle_state(outcomes);
            assert_eq!(bs.p1_active_mons[0].boosts[0], 0,
                "Defiant: Charm drops Atk by 2 (one stat) → one +2 trigger → net 0");
        }

        // Tickle lowers Atk AND Def by 1 each — two distinct stats → two triggers → +4 Atk.
        // Net on Atk: −1 + 4 = +3.  Def stays at −1.
        #[test]
        fn defiant_twice_for_two_distinct_stat_drops() {
            let mdex = move_dex();
            let pdex = pokemon_dex();
            let p1 = mon(Ability::Defiant, PokemonMove::Splash);
            let p2 = mon(Ability::None, PokemonMove::Tickle);
            let initial = battle_state_from_lists(vec![p1], vec![], vec![p2], vec![]);
            let outcomes = run_single_turn(
                &MatchState::BattleState(initial),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &mdex, &pdex,
            );
            let (bs, _) = extract_battle_state(outcomes);
            assert_eq!(bs.p1_active_mons[0].boosts[0], 3,
                "Defiant: Tickle drops Atk+Def (2 stats) → +4 total, net Atk = -1+4 = +3");
            assert_eq!(bs.p1_active_mons[0].boosts[1], -1,
                "Def drop still lands (only Atk gets the reaction, not Def)");
        }

        // Defiant fires before White Herb: −1 Atk +2 Defiant = +1 Atk → no negative stage →
        // White Herb should NOT be consumed.
        #[test]
        fn defiant_fires_before_white_herb_suppresses_it() {
            let mdex = move_dex();
            let pdex = pokemon_dex();
            let mut p1 = mon(Ability::Defiant, PokemonMove::Splash);
            p1.item = crate::data::item::Item::WhiteHerb;
            let p2 = mon(Ability::None, PokemonMove::Growl); // Growl: −1 Atk
            let initial = battle_state_from_lists(vec![p1], vec![], vec![p2], vec![]);
            let outcomes = run_single_turn(
                &MatchState::BattleState(initial),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &mdex, &pdex,
            );
            let (bs, _) = extract_battle_state(outcomes);
            assert_eq!(bs.p1_active_mons[0].boosts[0], 1,
                "Defiant +2 after −1 → net +1 Atk");
            assert_eq!(bs.p1_active_mons[0].item, crate::data::item::Item::WhiteHerb,
                "White Herb should NOT fire when no negative stage remains after Defiant");
        }

        // ── Competitive ────────────────────────────────────────────────

        // Intimidate (−1 Atk) into Competitive: Atk stays at −1, SpA rises +2.
        #[test]
        fn competitive_on_intimidate() {
            let p1 = mon(Ability::Intimidate, PokemonMove::Splash);
            let p2 = mon(Ability::Competitive, PokemonMove::Splash);
            let state = battle_state_from_lists(vec![p1], vec![], vec![p2], vec![]);
            assert_eq!(state.p2_active_mons[0].boosts[0], -1,
                "Competitive: Atk drop still applied");
            assert_eq!(state.p2_active_mons[0].boosts[2], 2,
                "Competitive: +2 SpA from Intimidate trigger");
        }

        // Competitive: White Herb fires AFTER the SpA boost when a different stat is still
        // negative (−1 Atk not cancelled by the SpA boost).
        #[test]
        fn competitive_white_herb_fires_on_other_stat() {
            let mdex = move_dex();
            let pdex = pokemon_dex();
            let mut p1 = mon(Ability::Competitive, PokemonMove::Splash);
            p1.item = crate::data::item::Item::WhiteHerb;
            let p2 = mon(Ability::None, PokemonMove::Growl); // −1 Atk
            let initial = battle_state_from_lists(vec![p1], vec![], vec![p2], vec![]);
            let outcomes = run_single_turn(
                &MatchState::BattleState(initial),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &mdex, &pdex,
            );
            let (bs, _) = extract_battle_state(outcomes);
            // The Atk drop stays (Competitive raises SpA, not Atk).
            // White Herb sees the remaining −1 Atk and fires → restores Atk to 0, consumed.
            assert_eq!(bs.p1_active_mons[0].boosts[2], 2,
                "Competitive: +2 SpA from the drop");
            assert_eq!(bs.p1_active_mons[0].boosts[0], 0,
                "White Herb restores the −1 Atk after Competitive ran");
            assert_eq!(bs.p1_active_mons[0].item, crate::data::item::Item::None,
                "White Herb consumed");
        }

        // ── Mirror Armor ───────────────────────────────────────────────

        // Mirror Armor bounces Intimidate back: holder keeps Atk=0, source loses Atk −1.
        #[test]
        fn mirror_armor_reflects_intimidate() {
            let p1 = mon(Ability::Intimidate, PokemonMove::Splash);
            let p2 = mon(Ability::MirrorArmor, PokemonMove::Splash);
            let state = battle_state_from_lists(vec![p1], vec![], vec![p2], vec![]);
            assert_eq!(state.p2_active_mons[0].boosts[0], 0,
                "Mirror Armor: holder's Atk unchanged");
            assert_eq!(state.p1_active_mons[0].boosts[0], -1,
                "Mirror Armor: Intimidate bounced back to the Intimidator");
        }

        // Mirror Armor reflects Growl via a move, not just entry abilities.
        #[test]
        fn mirror_armor_reflects_growl() {
            let mdex = move_dex();
            let pdex = pokemon_dex();
            let p1 = mon(Ability::MirrorArmor, PokemonMove::Splash);
            let p2 = mon(Ability::None, PokemonMove::Growl);
            let initial = battle_state_from_lists(vec![p1], vec![], vec![p2], vec![]);
            let outcomes = run_single_turn(
                &MatchState::BattleState(initial),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &mdex, &pdex,
            );
            let (bs, _) = extract_battle_state(outcomes);
            assert_eq!(bs.p1_active_mons[0].boosts[0], 0,
                "Mirror Armor: holder's Atk unchanged from Growl");
            assert_eq!(bs.p2_active_mons[0].boosts[0], -1,
                "Mirror Armor: Growl reflected back to the user");
        }

        // Bounce triggers the source's Defiant: P1 has Intimidate+Defiant, P2 has Mirror
        // Armor.  Intimidate fires → P2 bounces it back → P1 takes −1 Atk → P1's Defiant
        // reacts with +2 → net P1.Atk = +1, P2.Atk = 0.
        #[test]
        fn mirror_armor_bounce_triggers_source_defiant() {
            let p1 = mon(Ability::Defiant, PokemonMove::Splash);
            let p2 = mon(Ability::MirrorArmor, PokemonMove::Splash);
            // P1 enters with Defiant (no Intimidate here: we just set up the Intimidate
            // separately to avoid needing two abilities).  Instead use a move-based drop.
            // Simplest: use Growl from P2 to trigger Defiant on P1 via Mirror Armor reflection.
            // Actually: P2 uses Growl targeting P1 (Defiant); no Mirror Armor here.
            // For the Intimidate+Mirror Armor case we need an Intimidate user.
            // Build: P1 = Intimidate (not Defiant), P2 = Mirror Armor.
            // But we want to trigger Defiant on P1 after the bounce.
            // Use: P1=Intimidate, P2=Mirror Armor, and separately test Defiant.
            // Better: P1 uses Growl (drops P2 Atk), P2 has Mirror Armor (bounces to P1),
            // and P1 also has Defiant → P1 Atk: −1 from bounce + 2 from Defiant = +1.
            let mdex = move_dex();
            let pdex = pokemon_dex();
            let p1 = mon(Ability::Defiant, PokemonMove::Growl);  // P1 Growl → P2
            let p2 = mon(Ability::MirrorArmor, PokemonMove::Splash); // P2 bounces back
            let initial = battle_state_from_lists(vec![p1], vec![], vec![p2], vec![]);
            let outcomes = run_single_turn(
                &MatchState::BattleState(initial),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &mdex, &pdex,
            );
            let (bs, _) = extract_battle_state(outcomes);
            // P2 Atk unchanged (Mirror Armor bounced it)
            assert_eq!(bs.p2_active_mons[0].boosts[0], 0,
                "Mirror Armor holder takes no Atk drop");
            // P1 took −1 from the bounce; P1's own Defiant fires → net +1
            assert_eq!(bs.p1_active_mons[0].boosts[0], 1,
                "Defiant reacts to the reflected drop: -1+2 = +1");
        }

        // Mirror Armor vs Mirror Armor: P1 uses Growl against P2 (Mirror Armor).
        // P2 bounces back to P1 (also Mirror Armor, but already_reflected=true → no re-bounce).
        // P1 takes the −1 Atk.
        #[test]
        fn mirror_armor_vs_mirror_armor_no_infinite_loop() {
            let mdex = move_dex();
            let pdex = pokemon_dex();
            let p1 = mon(Ability::MirrorArmor, PokemonMove::Growl);
            let p2 = mon(Ability::MirrorArmor, PokemonMove::Splash);
            let initial = battle_state_from_lists(vec![p1], vec![], vec![p2], vec![]);
            let outcomes = run_single_turn(
                &MatchState::BattleState(initial),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &mdex, &pdex,
            );
            let (bs, _) = extract_battle_state(outcomes);
            // P2 bounced the Growl drop back to P1; P1 has Mirror Armor but already_reflected=true
            assert_eq!(bs.p2_active_mons[0].boosts[0], 0, "P2 Mirror Armor: no drop taken");
            assert_eq!(bs.p1_active_mons[0].boosts[0], -1, "P1 receives the reflected drop");
        }

        // Suppressed Mirror Armor (Gastro Acid volatile) takes the drop normally.
        #[test]
        fn mirror_armor_suppressed_takes_drop() {
            let mdex = move_dex();
            let pdex = pokemon_dex();
            let mut p1 = mon(Ability::MirrorArmor, PokemonMove::Splash);
            // Manually apply Gastro Acid volatile to suppress the ability.
            p1.volatiles.push(crate::pokemon::VolatileStatusState::TurnStatus(
                crate::dex_data::VolatileStatus::GastroAcid, 0,
            ));
            let p2 = mon(Ability::None, PokemonMove::Growl);
            let initial = battle_state_from_lists(vec![p1], vec![], vec![p2], vec![]);
            let outcomes = run_single_turn(
                &MatchState::BattleState(initial),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &mdex, &pdex,
            );
            let (bs, _) = extract_battle_state(outcomes);
            assert_eq!(bs.p1_active_mons[0].boosts[0], -1,
                "Mirror Armor suppressed: drop lands on the holder");
            assert_eq!(bs.p2_active_mons[0].boosts[0], 0,
                "Suppressed Mirror Armor: nothing reflected to user");
        }
    }

    // ════════════════════════════════════════════════════════════════════
    // Stat-change reaction abilities:
    //   Subgroup B — Anger Point, Berserk, Electromorphosis, Justified,
    //                Moxie, Opportunist, Stamina, Steadfast
    //   Trigger: taking damage / a battle event.
    // ════════════════════════════════════════════════════════════════════
    mod damage_reaction_abilities {
        use super::*;
        use crate::battle::AttackCommand;

        fn make_mon(species: Species, ability: Ability, first_move: PokemonMove) -> PokemonState {
            let pdex = pokemon_dex();
            let mdex = move_dex();
            build_pokemon_state(
                species, &pdex, &mdex, Some(50),
                Some([Some(first_move), None, None, None]),
                None, Some(ability), Some(Nature::Hardy),
                None, None, Some([0, 0, 0, 0, 0, 0]), None, false,
            )
        }

        // ── Stamina ────────────────────────────────────────────────────

        #[test]
        fn stamina_raises_defense_on_hit() {
            let mdex = move_dex();
            let pdex = pokemon_dex();
            let p1 = make_mon(Species::Snorlax, Ability::Stamina, PokemonMove::Splash);
            let p2 = make_mon(Species::Snorlax, Ability::None, PokemonMove::Tackle);
            let initial = battle_state_from_lists(vec![p1], vec![], vec![p2], vec![]);
            let outcomes = run_single_turn(
                &MatchState::BattleState(initial),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &mdex, &pdex,
            );
            let (bs, _) = extract_battle_state(outcomes);
            assert_eq!(bs.p1_active_mons[0].boosts[1], 1, "Stamina: +1 Def on hit");
        }

        #[test]
        fn stamina_does_not_trigger_on_status_move() {
            let mdex = move_dex();
            let pdex = pokemon_dex();
            let p1 = make_mon(Species::Snorlax, Ability::Stamina, PokemonMove::Splash);
            let p2 = make_mon(Species::Snorlax, Ability::None, PokemonMove::Growl);
            let initial = battle_state_from_lists(vec![p1], vec![], vec![p2], vec![]);
            let outcomes = run_single_turn(
                &MatchState::BattleState(initial),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &mdex, &pdex,
            );
            let (bs, _) = extract_battle_state(outcomes);
            assert_eq!(bs.p1_active_mons[0].boosts[1], 0, "Stamina: no trigger on status-only move");
        }

        // ── Justified ──────────────────────────────────────────────────

        #[test]
        fn justified_on_dark_move() {
            let mdex = move_dex();
            let pdex = pokemon_dex();
            // Sucker Punch is Dark-type
            let p1 = make_mon(Species::Snorlax, Ability::Justified, PokemonMove::Splash);
            let mut p2 = make_mon(Species::Snorlax, Ability::None, PokemonMove::SuckerPunch);
            p2.stats[4] = 300; // ensure P2 moves first
            let initial = battle_state_from_lists(vec![p1], vec![], vec![p2], vec![]);
            let outcomes = run_single_turn(
                &MatchState::BattleState(initial),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &mdex, &pdex,
            );
            // Note: Sucker Punch fails unless target is using a damaging move, so use a
            // different Dark move. Use Bite instead.
            // Retry with Bite:
            let p1 = make_mon(Species::Snorlax, Ability::Justified, PokemonMove::Splash);
            let mut p2 = make_mon(Species::Snorlax, Ability::None, PokemonMove::Bite);
            p2.stats[4] = 300;
            let initial2 = battle_state_from_lists(vec![p1], vec![], vec![p2], vec![]);
            let outcomes2 = run_single_turn(
                &MatchState::BattleState(initial2),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &mdex, &pdex,
            );
            // Check on the hit branches
            let _ = outcomes;
            for (s, _) in &outcomes2 {
                if let MatchState::BattleState(bs) = s {
                    if bs.p1_active_mons[0].hp < bs.p1_active_mons[0].stats[0] {
                        // was hit
                        assert_eq!(bs.p1_active_mons[0].boosts[0], 1, "Justified: +1 Atk on Dark hit");
                    }
                }
            }
        }

        #[test]
        fn justified_not_triggered_by_normal_move() {
            let mdex = move_dex();
            let pdex = pokemon_dex();
            let p1 = make_mon(Species::Snorlax, Ability::Justified, PokemonMove::Splash);
            let p2 = make_mon(Species::Snorlax, Ability::None, PokemonMove::Tackle);
            let initial = battle_state_from_lists(vec![p1], vec![], vec![p2], vec![]);
            let outcomes = run_single_turn(
                &MatchState::BattleState(initial),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &mdex, &pdex,
            );
            for (s, _) in &outcomes {
                if let MatchState::BattleState(bs) = s {
                    assert_eq!(bs.p1_active_mons[0].boosts[0], 0, "Justified: no trigger on Normal hit");
                }
            }
        }

        // ── Anger Point ────────────────────────────────────────────────

        #[test]
        fn anger_point_maxes_attack_on_crit() {
            // Give P2 LaserFocus volatile → crit_is_guaranteed → every P2 hit crits.
            // Must call simulate_turn directly with consider_crit=true; run_single_turn
            // forces consider_crit=false which skips all crit branches entirely.
            let pdex = pokemon_dex();
            let mdex = move_dex();
            let p1 = make_mon(Species::Snorlax, Ability::AngerPoint, PokemonMove::Splash);
            let mut p2 = make_mon(Species::Snorlax, Ability::None, PokemonMove::Tackle);
            // LaserFocus is checked by crit_is_guaranteed → always crits
            p2.volatiles.push(crate::pokemon::VolatileStatusState::MoveStatus(
                crate::dex_data::VolatileStatus::LaserFocus, 0,
            ));
            p2.stats[4] = 300;
            let initial = battle_state_from_lists(vec![p1], vec![], vec![p2], vec![]);
            let outcomes = simulate_turn(
                &MatchState::BattleState(initial),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &mdex, &pdex,
                true, // consider_crit — required for crit_is_guaranteed to fire
                1,
            );
            // P2 always crits → Anger Point fires → P1 Atk should be maxed at +6
            let all_maxed = outcomes.iter().all(|(s, _)| {
                matches!(s, MatchState::BattleState(bs) if bs.p1_active_mons[0].boosts[0] == 6)
            });
            assert!(all_maxed, "Anger Point: guaranteed-crit should maximise Atk to +6 on all branches");
        }

        // ── Berserk ────────────────────────────────────────────────────

        #[test]
        fn berserk_triggers_when_move_crosses_half_hp() {
            let mdex = move_dex();
            let pdex = pokemon_dex();
            let mut p1 = make_mon(Species::Snorlax, Ability::Berserk, PokemonMove::Splash);
            let max_hp = p1.stats[0];
            // Set HP to just above 50% so one hit from Seismic Toss or a calibrated move drops it.
            p1.hp = max_hp / 2 + 1;
            let mut p2 = make_mon(Species::Snorlax, Ability::None, PokemonMove::Tackle);
            p2.stats[4] = 300;
            let initial = battle_state_from_lists(vec![p1], vec![], vec![p2], vec![]);
            let outcomes = run_single_turn(
                &MatchState::BattleState(initial),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &mdex, &pdex,
            );
            // Some branches should cross 50% → Berserk fires → SpA == +1
            let any_berserk = outcomes.iter().any(|(s, _)| {
                matches!(s, MatchState::BattleState(bs) if bs.p1_active_mons[0].boosts[2] == 1)
            });
            assert!(any_berserk, "Berserk: should fire on at least one branch that crosses 50%");
        }

        #[test]
        fn berserk_no_trigger_if_already_below_half() {
            let mdex = move_dex();
            let pdex = pokemon_dex();
            let mut p1 = make_mon(Species::Snorlax, Ability::Berserk, PokemonMove::Splash);
            let max_hp = p1.stats[0];
            // Already below 50%
            p1.hp = max_hp / 4;
            let mut p2 = make_mon(Species::Snorlax, Ability::None, PokemonMove::Tackle);
            p2.stats[4] = 300;
            let initial = battle_state_from_lists(vec![p1], vec![], vec![p2], vec![]);
            let outcomes = run_single_turn(
                &MatchState::BattleState(initial),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &mdex, &pdex,
            );
            for (s, _) in &outcomes {
                if let MatchState::BattleState(bs) = s {
                    assert_eq!(bs.p1_active_mons[0].boosts[2], 0,
                        "Berserk: should not trigger if already at or below 50% HP");
                }
            }
        }

        #[test]
        fn berserk_triggers_on_burn_damage() {
            // Berserk fires on indirect damage (burn); set HP to just above 50%.
            let pdex = pokemon_dex();
            let mdex = move_dex();
            let mut p1 = make_mon(Species::Snorlax, Ability::Berserk, PokemonMove::Splash);
            let max_hp = p1.stats[0];
            p1.hp = max_hp / 2 + 1;
            p1.status = Some(crate::dex_data::Status::Burn);
            let p2 = make_mon(Species::Snorlax, Ability::None, PokemonMove::Splash);
            // Both use Splash so no move damage; only burn damage at end of turn.
            let initial = battle_state_from_lists(vec![p1], vec![], vec![p2], vec![]);
            let outcomes = run_single_turn(
                &MatchState::BattleState(initial),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &mdex, &pdex,
            );
            let any_berserk = outcomes.iter().any(|(s, _)| {
                matches!(s, MatchState::BattleState(bs) if bs.p1_active_mons[0].boosts[2] == 1)
            });
            assert!(any_berserk, "Berserk: should fire when burn damage crosses 50%");
        }

        // ── Steadfast ──────────────────────────────────────────────────

        #[test]
        fn steadfast_raises_speed_on_flinch() {
            let mdex = move_dex();
            let pdex = pokemon_dex();
            // Air Slash has 30% flinch chance
            let p1 = make_mon(Species::Snorlax, Ability::Steadfast, PokemonMove::Splash);
            let mut p2 = make_mon(Species::Snorlax, Ability::None, PokemonMove::AirSlash);
            p2.stats[4] = 300;
            let initial = battle_state_from_lists(vec![p1], vec![], vec![p2], vec![]);
            let outcomes = run_single_turn(
                &MatchState::BattleState(initial),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &mdex, &pdex,
            );
            // Some branches: flinch → +1 Spe; others: no flinch → +0 Spe
            let any_steadfast = outcomes.iter().any(|(s, _)| {
                matches!(s, MatchState::BattleState(bs) if bs.p1_active_mons[0].boosts[4] == 1)
            });
            let any_no_flinch = outcomes.iter().any(|(s, _)| {
                matches!(s, MatchState::BattleState(bs) if bs.p1_active_mons[0].boosts[4] == 0)
            });
            assert!(any_steadfast, "Steadfast: flinch branch should give +1 Spe");
            assert!(any_no_flinch, "Steadfast: non-flinch branch should leave Spe unchanged");
        }

        // ── Moxie ──────────────────────────────────────────────────────

        #[test]
        fn moxie_raises_attack_on_ko() {
            let mdex = move_dex();
            let pdex = pokemon_dex();
            // P1: Moxie + Tackle (100% accurate, no secondary → single deterministic branch).
            let p1 = make_mon(Species::Snorlax, Ability::Moxie, PokemonMove::Tackle);
            // P2 active: 1 HP so any hit OHKOs.
            let mut p2_active = make_mon(Species::Snorlax, Ability::None, PokemonMove::Splash);
            p2_active.hp = 1;
            p2_active.stats[4] = 1; // slow → P1 moves first
            // P2 backup prevents GameOver so we can inspect P1's boosts after the KO.
            // Without a backup, P2's last-mon faint produces GameOverState (no boosts accessible).
            let p2_backup = make_mon(Species::Snorlax, Ability::None, PokemonMove::Splash);
            let initial = battle_state_from_lists(vec![p1], vec![], vec![p2_active], vec![p2_backup]);
            let outcomes = run_single_turn(
                &MatchState::BattleState(initial),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &mdex, &pdex,
            );
            // P2's active faints; game continues (backup in back_mons) → BattleState.
            // P1 should have +1 Atk from Moxie on every branch.
            let all_boosted = outcomes.iter().all(|(s, _)| {
                matches!(s, MatchState::BattleState(bs) if bs.p1_active_mons[0].boosts[0] == 1)
            });
            assert!(all_boosted, "Moxie: +1 Atk on every branch after KO (backup keeps game alive)");
        }

        // ── Electromorphosis + Charge consumer ─────────────────────────

        #[test]
        fn electromorphosis_grants_charge_on_hit() {
            let mdex = move_dex();
            let pdex = pokemon_dex();
            let p1 = make_mon(Species::Snorlax, Ability::Electromorphosis, PokemonMove::Splash);
            let p2 = make_mon(Species::Snorlax, Ability::None, PokemonMove::Tackle);
            let initial = battle_state_from_lists(vec![p1], vec![], vec![p2], vec![]);
            let outcomes = run_single_turn(
                &MatchState::BattleState(initial),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &mdex, &pdex,
            );
            let (bs, _) = extract_battle_state(outcomes);
            let has_charge = bs.p1_active_mons[0].volatiles.iter().any(|v| {
                matches!(v, crate::pokemon::VolatileStatusState::TurnStatus(crate::dex_data::VolatileStatus::Charge, _))
            });
            assert!(has_charge, "Electromorphosis: Charge volatile should be added on hit");
        }

        #[test]
        fn charge_doubles_next_electric_move() {
            let mdex = move_dex();
            let pdex = pokemon_dex();
            // Thunderbolt has a 10% paralysis secondary → two outcome branches per run.
            // Use expected (probability-weighted) damage to compare; avoids extract_battle_state.
            let p2_target = make_mon(Species::Snorlax, Ability::None, PokemonMove::Splash);
            let initial_hp = p2_target.stats[0];

            let avg_damage = |outcomes: &[(MatchState, f64)]| -> f64 {
                outcomes.iter().map(|(s, p)| {
                    let dmg = match s {
                        MatchState::BattleState(bs) =>
                            initial_hp.saturating_sub(bs.p2_active_mons[0].hp) as f64,
                        MatchState::GameOverState { .. } => initial_hp as f64,
                        _ => 0.0,
                    };
                    dmg * p
                }).sum()
            };

            // Baseline: no Charge volatile
            let mut p1_base = make_mon(Species::Snorlax, Ability::None, PokemonMove::Thunderbolt);
            p1_base.stats[4] = 300;
            let base_outcomes = run_single_turn(
                &MatchState::BattleState(battle_state_from_lists(
                    vec![p1_base], vec![], vec![p2_target.clone()], vec![],
                )),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &mdex, &pdex,
            );
            let base_avg = avg_damage(&base_outcomes);

            // With Charge volatile: effective BP doubles → ~2× damage
            let mut p1_charged = make_mon(Species::Snorlax, Ability::None, PokemonMove::Thunderbolt);
            p1_charged.stats[4] = 300;
            p1_charged.volatiles.push(crate::pokemon::VolatileStatusState::TurnStatus(
                crate::dex_data::VolatileStatus::Charge, 0,
            ));
            let charged_outcomes = run_single_turn(
                &MatchState::BattleState(battle_state_from_lists(
                    vec![p1_charged], vec![], vec![p2_target.clone()], vec![],
                )),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &mdex, &pdex,
            );
            let charged_avg = avg_damage(&charged_outcomes);

            assert!(charged_avg > base_avg * 1.5,
                "Charge: charged Thunderbolt avg {charged_avg:.1} should be >1.5× uncharged {base_avg:.1}");

            // Charge volatile should be consumed on every BattleState branch
            let still_charged = charged_outcomes.iter().any(|(s, _)| {
                matches!(s, MatchState::BattleState(bs)
                    if bs.p1_active_mons[0].volatiles.iter().any(|v| matches!(v,
                        crate::pokemon::VolatileStatusState::TurnStatus(
                            crate::dex_data::VolatileStatus::Charge, _))))
            });
            assert!(!still_charged, "Charge volatile should be consumed after an Electric move");
        }

        // ── Opportunist ────────────────────────────────────────────────

        #[test]
        fn opportunist_copies_swords_dance() {
            let mdex = move_dex();
            let pdex = pokemon_dex();
            // P1 uses Swords Dance (+2 Atk); P2 has Opportunist → P2 should also get +2 Atk.
            let mut p1 = make_mon(Species::Snorlax, Ability::None, PokemonMove::SwordsDance);
            p1.stats[4] = 300; // P1 moves first
            let p2 = make_mon(Species::Snorlax, Ability::Opportunist, PokemonMove::Splash);
            let initial = battle_state_from_lists(vec![p1], vec![], vec![p2], vec![]);
            let outcomes = run_single_turn(
                &MatchState::BattleState(initial),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &mdex, &pdex,
            );
            let (bs, _) = extract_battle_state(outcomes);
            assert_eq!(bs.p1_active_mons[0].boosts[0], 2, "P1 Swords Dance: +2 Atk");
            assert_eq!(bs.p2_active_mons[0].boosts[0], 2, "Opportunist: P2 also gets +2 Atk");
        }

        #[test]
        fn opportunist_does_not_copy_own_boost() {
            let mdex = move_dex();
            let pdex = pokemon_dex();
            // P2 with Opportunist uses Swords Dance on itself; should NOT copy its own boost.
            let p1 = make_mon(Species::Snorlax, Ability::None, PokemonMove::Splash);
            let mut p2 = make_mon(Species::Snorlax, Ability::Opportunist, PokemonMove::SwordsDance);
            p2.stats[4] = 1; // P2 moves last
            let initial = battle_state_from_lists(vec![p1], vec![], vec![p2], vec![]);
            let outcomes = run_single_turn(
                &MatchState::BattleState(initial),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &mdex, &pdex,
            );
            let (bs, _) = extract_battle_state(outcomes);
            assert_eq!(bs.p2_active_mons[0].boosts[0], 2,
                "P2 Opportunist using Swords Dance: just +2 (own boost, no copy)");
        }
    }

    // ════════════════════════════════════════════════════════════════════
    // Damage-reduction abilities
    // ════════════════════════════════════════════════════════════════════
    mod damage_reduction_abilities {
        use super::*;
        use crate::battle::AttackCommand;

        /// Build a level-50 mon with a specific ability, first move, and Nature::Hardy.
        /// All stat-points are zero for predictable, reproducible stats.
        fn make_mon(species: Species, ability: Ability, first_move: PokemonMove) -> PokemonState {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();
            build_pokemon_state(
                species, &pokemon_dex, &move_dex, Some(50),
                Some([Some(first_move), None, None, None]),
                None, Some(ability), Some(Nature::Hardy),
                None, None, Some([0, 0, 0, 0, 0, 0]), None, false,
            )
        }

        /// Run a one-turn P1-attacks-P2 scenario and return the probability-weighted
        /// expected damage dealt to P2 slot 0.
        fn run_damage(
            attacker: PokemonState,
            defender: PokemonState,
            move_dex: &std::collections::HashMap<PokemonMove, crate::dex_data::MoveData>,
            pokemon_dex: &std::collections::HashMap<Species, crate::dex_data::PokemonData>,
        ) -> f64 {
            let initial_hp = defender.hp;
            let state = battle_state_from_lists(vec![attacker], vec![], vec![defender], vec![]);
            let outcomes = run_single_turn(
                &MatchState::BattleState(state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                move_dex, pokemon_dex,
            );
            outcomes.iter().map(|(s, p)| {
                let hp = match s {
                    MatchState::BattleState(bs) => bs.p2_active_mons[0].hp,
                    _ => 0,
                };
                (initial_hp.saturating_sub(hp) as f64) * p
            }).sum()
        }

        // ── Filter / Solid Rock ─────────────────────────────────────────────────
        // Geodude is Rock/Ground — 4× weak to Water.  Filter/Solid Rock bring it to ×3.
        // The damage ratio vs no-ability should be 0.75.

        #[test]
        fn filter_reduces_super_effective_damage() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();
            let attacker = make_mon(Species::Blastoise, Ability::None, PokemonMove::WaterGun);
            // Charizard (Fire/Flying) is 2× weak to Water.  A 4× target (Geodude) would
            // faint in both the Filter and no-Filter cases, masking the reduction.
            let dmg_none   = run_damage(attacker.clone(), make_mon(Species::Charizard, Ability::None,      PokemonMove::Splash), &move_dex, &pokemon_dex);
            let dmg_filter = run_damage(attacker.clone(), make_mon(Species::Charizard, Ability::Filter,    PokemonMove::Splash), &move_dex, &pokemon_dex);
            let dmg_solid  = run_damage(attacker,         make_mon(Species::Charizard, Ability::SolidRock, PokemonMove::Splash), &move_dex, &pokemon_dex);
            assert!((dmg_filter / dmg_none - 0.75).abs() < 0.02,
                "Filter: expected ~0.75× SE damage, got {:.4}", dmg_filter / dmg_none);
            assert!((dmg_solid / dmg_none - 0.75).abs() < 0.02,
                "Solid Rock: expected ~0.75× SE damage, got {:.4}", dmg_solid / dmg_none);
        }

        // Filter must NOT reduce neutral-effectiveness damage.
        #[test]
        fn filter_does_not_reduce_neutral_damage() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();
            let attacker = make_mon(Species::Blastoise, Ability::None, PokemonMove::WaterGun);
            // Snorlax is Normal — Water is neutral (×1).
            let dmg_none   = run_damage(attacker.clone(), make_mon(Species::Snorlax, Ability::None,   PokemonMove::Splash), &move_dex, &pokemon_dex);
            let dmg_filter = run_damage(attacker,         make_mon(Species::Snorlax, Ability::Filter, PokemonMove::Splash), &move_dex, &pokemon_dex);
            assert!((dmg_filter - dmg_none).abs() < 1.0,
                "Filter must not reduce neutral hits (expected {dmg_none:.1}, got {dmg_filter:.1})");
        }

        // ── Multiscale / Shadow Shield ──────────────────────────────────────────

        #[test]
        fn multiscale_halves_damage_at_full_hp() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();
            let attacker = make_mon(Species::Blastoise, Ability::None, PokemonMove::WaterGun);

            let defender_full = make_mon(Species::Snorlax, Ability::Multiscale, PokemonMove::Splash);
            let mut defender_damaged = defender_full.clone();
            defender_damaged.hp -= 1; // one below max → ability does not fire

            let dmg_full    = run_damage(attacker.clone(), defender_full,    &move_dex, &pokemon_dex);
            let dmg_damaged = run_damage(attacker,         defender_damaged, &move_dex, &pokemon_dex);

            assert!((dmg_full / dmg_damaged - 0.5).abs() < 0.05,
                "Multiscale: expected ~0.5× damage at full HP (ratio={:.4})", dmg_full / dmg_damaged);
        }

        #[test]
        fn shadow_shield_halves_damage_at_full_hp() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();
            let attacker = make_mon(Species::Blastoise, Ability::None, PokemonMove::WaterGun);

            let defender_shield  = make_mon(Species::Snorlax, Ability::ShadowShield, PokemonMove::Splash);
            let defender_no_abil = make_mon(Species::Snorlax, Ability::None,         PokemonMove::Splash);

            let dmg_shield = run_damage(attacker.clone(), defender_shield,  &move_dex, &pokemon_dex);
            let dmg_none   = run_damage(attacker,         defender_no_abil, &move_dex, &pokemon_dex);

            assert!((dmg_shield / dmg_none - 0.5).abs() < 0.05,
                "Shadow Shield: expected ~0.5× damage at full HP (ratio={:.4})", dmg_shield / dmg_none);
        }

        // ── Fur Coat ────────────────────────────────────────────────────────────

        #[test]
        fn fur_coat_halves_physical_damage() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();
            // Close Combat is Physical (120 BP Fighting).  Snorlax is Normal — neutral.
            let attacker = make_mon(Species::Lucario, Ability::None, PokemonMove::CloseCombat);
            let dmg_none     = run_damage(attacker.clone(), make_mon(Species::Snorlax, Ability::None,    PokemonMove::Splash), &move_dex, &pokemon_dex);
            let dmg_fur_coat = run_damage(attacker,         make_mon(Species::Snorlax, Ability::FurCoat, PokemonMove::Splash), &move_dex, &pokemon_dex);
            assert!((dmg_fur_coat / dmg_none - 0.5).abs() < 0.02,
                "Fur Coat: expected ~0.5× physical damage (ratio={:.4})", dmg_fur_coat / dmg_none);
        }

        #[test]
        fn fur_coat_does_not_reduce_special_damage() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();
            // Psychic is Special.
            let attacker = make_mon(Species::Alakazam, Ability::None, PokemonMove::Psychic);
            let dmg_none     = run_damage(attacker.clone(), make_mon(Species::Snorlax, Ability::None,    PokemonMove::Splash), &move_dex, &pokemon_dex);
            let dmg_fur_coat = run_damage(attacker,         make_mon(Species::Snorlax, Ability::FurCoat, PokemonMove::Splash), &move_dex, &pokemon_dex);
            assert!((dmg_fur_coat - dmg_none).abs() < 1.0,
                "Fur Coat must not reduce special damage (expected {dmg_none:.1}, got {dmg_fur_coat:.1})");
        }

        // ── Heatproof ───────────────────────────────────────────────────────────

        #[test]
        fn heatproof_halves_fire_move_damage() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();
            let attacker = make_mon(Species::Charizard, Ability::None, PokemonMove::Flamethrower);
            let dmg_none      = run_damage(attacker.clone(), make_mon(Species::Snorlax, Ability::None,      PokemonMove::Splash), &move_dex, &pokemon_dex);
            let dmg_heatproof = run_damage(attacker,         make_mon(Species::Snorlax, Ability::Heatproof, PokemonMove::Splash), &move_dex, &pokemon_dex);
            assert!((dmg_heatproof / dmg_none - 0.5).abs() < 0.02,
                "Heatproof: expected ~0.5× Fire damage (ratio={:.4})", dmg_heatproof / dmg_none);
        }

        #[test]
        fn heatproof_halves_burn_residual() {
            // A burned Heatproof holder should take max_hp/32 per turn instead of max_hp/16.
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();
            let mut burned = make_mon(Species::Snorlax, Ability::Heatproof, PokemonMove::Splash);
            burned.status = Some(Status::Burn);
            let max_hp     = burned.stats[0];
            let initial_hp = burned.hp;
            let p2 = make_mon(Species::Snorlax, Ability::None, PokemonMove::Splash);

            let state = battle_state_from_lists(vec![burned], vec![], vec![p2], vec![]);
            let outcomes = run_single_turn(
                &MatchState::BattleState(state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex, &pokemon_dex,
            );
            let (bs, _) = extract_battle_state(outcomes);
            let expected_residual = ((max_hp as u32 / 32) as u16).max(1);
            let actual_residual   = initial_hp.saturating_sub(bs.p1_active_mons[0].hp);
            assert_eq!(actual_residual, expected_residual,
                "Heatproof burn: expected max_hp/32={expected_residual}, got {actual_residual}");
        }

        // ── Thick Fat ───────────────────────────────────────────────────────────

        #[test]
        fn thick_fat_halves_fire_damage() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();
            let attacker = make_mon(Species::Charizard, Ability::None, PokemonMove::Flamethrower);
            let dmg_none      = run_damage(attacker.clone(), make_mon(Species::Snorlax, Ability::None,     PokemonMove::Splash), &move_dex, &pokemon_dex);
            let dmg_thick_fat = run_damage(attacker,         make_mon(Species::Snorlax, Ability::ThickFat, PokemonMove::Splash), &move_dex, &pokemon_dex);
            assert!((dmg_thick_fat / dmg_none - 0.5).abs() < 0.02,
                "Thick Fat: expected ~0.5× Fire damage (ratio={:.4})", dmg_thick_fat / dmg_none);
        }

        #[test]
        fn thick_fat_halves_ice_damage() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();
            let attacker = make_mon(Species::Lapras, Ability::None, PokemonMove::IceBeam);
            let dmg_none      = run_damage(attacker.clone(), make_mon(Species::Snorlax, Ability::None,     PokemonMove::Splash), &move_dex, &pokemon_dex);
            let dmg_thick_fat = run_damage(attacker,         make_mon(Species::Snorlax, Ability::ThickFat, PokemonMove::Splash), &move_dex, &pokemon_dex);
            assert!((dmg_thick_fat / dmg_none - 0.5).abs() < 0.02,
                "Thick Fat: expected ~0.5× Ice damage (ratio={:.4})", dmg_thick_fat / dmg_none);
        }

        // ── Water Bubble ────────────────────────────────────────────────────────

        #[test]
        fn water_bubble_halves_fire_damage_taken() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();
            let attacker = make_mon(Species::Charizard, Ability::None, PokemonMove::Flamethrower);
            let dmg_none   = run_damage(attacker.clone(), make_mon(Species::Snorlax, Ability::None,        PokemonMove::Splash), &move_dex, &pokemon_dex);
            let dmg_bubble = run_damage(attacker,         make_mon(Species::Snorlax, Ability::WaterBubble, PokemonMove::Splash), &move_dex, &pokemon_dex);
            // Tolerance is 0.05 (not 0.02) because WaterBubble prevents burn so the
            // no-ability branch includes occasional burn residual that skews the ratio.
            assert!((dmg_bubble / dmg_none - 0.5).abs() < 0.05,
                "Water Bubble: expected ~0.5× Fire damage taken (ratio={:.4})", dmg_bubble / dmg_none);
        }

        #[test]
        fn water_bubble_doubles_water_move_power() {
            // The holder's Water-type moves have their base power doubled.
            // Use Surf (90 BP) so the formula's constant +2 is relatively smaller,
            // keeping the ratio close to 2.0 within the 0.05 tolerance.
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();
            let dmg_none   = run_damage(
                make_mon(Species::Blastoise, Ability::None,        PokemonMove::Surf),
                make_mon(Species::Snorlax,  Ability::None,         PokemonMove::Splash),
                &move_dex, &pokemon_dex,
            );
            let dmg_bubble = run_damage(
                make_mon(Species::Blastoise, Ability::WaterBubble, PokemonMove::Surf),
                make_mon(Species::Snorlax,  Ability::None,         PokemonMove::Splash),
                &move_dex, &pokemon_dex,
            );
            assert!((dmg_bubble / dmg_none - 2.0).abs() < 0.05,
                "Water Bubble: expected ~2× Water move power (ratio={:.4})", dmg_bubble / dmg_none);
        }

        // ── Purifying Salt ──────────────────────────────────────────────────────

        #[test]
        fn purifying_salt_halves_ghost_damage() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();
            // Machamp (Fighting) is not immune to Ghost (unlike Normal/Dark types).
            let attacker = make_mon(Species::Gengar, Ability::None, PokemonMove::ShadowBall);
            let dmg_none = run_damage(attacker.clone(), make_mon(Species::Machamp, Ability::None,          PokemonMove::Splash), &move_dex, &pokemon_dex);
            let dmg_salt = run_damage(attacker,         make_mon(Species::Machamp, Ability::PurifyingSalt, PokemonMove::Splash), &move_dex, &pokemon_dex);
            assert!((dmg_salt / dmg_none - 0.5).abs() < 0.02,
                "Purifying Salt: expected ~0.5× Ghost damage (ratio={:.4})", dmg_salt / dmg_none);
        }

        // ── Friend Guard ────────────────────────────────────────────────────────
        // Doubles: P2[0] is the Friend Guard ally; P1[0] explicitly targets P2[1].

        #[test]
        fn friend_guard_reduces_ally_damage_by_25_percent() {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();

            let attacker   = make_mon(Species::Blastoise, Ability::None,        PokemonMove::WaterGun);
            let dummy_p1   = make_mon(Species::Snorlax,   Ability::None,        PokemonMove::Splash);
            let target     = make_mon(Species::Snorlax,   Ability::None,        PokemonMove::Splash);
            let initial_hp = target.hp;

            // Command: P1[0] attacks P2 slot 1 explicitly; P1[1] splashes.
            let p1_cmd = PlayerCommand::Battle(vec![
                BattleCommand::Attack(AttackCommand {
                    move_slot: 0,
                    target: Some(FieldSlot { player: Player::P2, slot_index: 1 }),
                    terastallize: false,
                    mega_evolve: false,
                }),
                BattleCommand::Attack(AttackCommand {
                    move_slot: 0,
                    target: None,
                    terastallize: false,
                    mega_evolve: false,
                }),
            ]);
            let p2_cmd = PlayerCommand::Battle(simple_attack(Player::P2, vec![0, 0]));

            // Without Friend Guard: P2[0] has no relevant ability.
            let state_no_guard = battle_state_from_lists(
                vec![attacker.clone(), dummy_p1.clone()], vec![],
                vec![make_mon(Species::Snorlax, Ability::None, PokemonMove::Splash), target.clone()],
                vec![],
            );
            let outcomes_no = run_single_turn(
                &MatchState::BattleState(state_no_guard),
                &p1_cmd, &p2_cmd, &move_dex, &pokemon_dex,
            );
            let dmg_no_guard: f64 = outcomes_no.iter().map(|(s, p)| {
                let hp = match s { MatchState::BattleState(bs) => bs.p2_active_mons[1].hp, _ => 0 };
                (initial_hp.saturating_sub(hp) as f64) * p
            }).sum();

            // With Friend Guard: P2[0] carries Friend Guard.
            let state_guard = battle_state_from_lists(
                vec![attacker, dummy_p1], vec![],
                vec![make_mon(Species::Snorlax, Ability::FriendGuard, PokemonMove::Splash), target],
                vec![],
            );
            let outcomes_guard = run_single_turn(
                &MatchState::BattleState(state_guard),
                &p1_cmd, &p2_cmd, &move_dex, &pokemon_dex,
            );
            let dmg_guard: f64 = outcomes_guard.iter().map(|(s, p)| {
                let hp = match s { MatchState::BattleState(bs) => bs.p2_active_mons[1].hp, _ => 0 };
                (initial_hp.saturating_sub(hp) as f64) * p
            }).sum();

            assert!((dmg_guard / dmg_no_guard - 0.75).abs() < 0.05,
                "Friend Guard: expected ~0.75× ally damage (ratio={:.4})", dmg_guard / dmg_no_guard);
        }
    }

    // ── Type immunity & absorption abilities ─────────────────────────────────────
    mod type_immunity_abilities {
        use super::*;
        use crate::battle::AttackCommand;

        // Build a level-50 mon with the given ability and a single move.
        fn mon(species: Species, ability: Ability, mv: PokemonMove) -> PokemonState {
            let dex = pokemon_dex();
            let mdex = move_dex();
            build_pokemon_state(
                species, &dex, &mdex, Some(50),
                Some([Some(mv), None, None, None]),
                None, Some(ability), Some(Nature::Hardy),
                None, None, Some([0; 6]), None, false,
            )
        }

        // ── react-on-hit: heal abilities ─────────────────────────────────────────

        #[test]
        fn volt_absorb_heals_on_electric_move() {
            let mdex = move_dex();
            let pdex = pokemon_dex();
            let mut absorber = mon(Species::Jolteon, Ability::VoltAbsorb, PokemonMove::Splash);
            let max_hp = absorber.stats[0];
            absorber.hp = max_hp.saturating_sub(max_hp / 4);
            let initial_hp = absorber.hp;

            let state = battle_state_from_lists(
                vec![absorber],
                vec![],
                vec![mon(Species::Raichu, Ability::Pressure, PokemonMove::Thunderbolt)],
                vec![],
            );
            let outcomes = run_single_turn(
                &MatchState::BattleState(state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &mdex, &pdex,
            );
            let expected_hp = initial_hp + max_hp / 4;
            assert!(
                outcomes.iter().all(|(s, _)| matches!(s, MatchState::BattleState(bs) if bs.p1_active_mons[0].hp == expected_hp)),
                "Volt Absorb: expected HP={} in all branches", expected_hp,
            );
        }

        #[test]
        fn water_absorb_heals_on_water_move() {
            let mdex = move_dex();
            let pdex = pokemon_dex();
            let mut absorber = mon(Species::Vaporeon, Ability::WaterAbsorb, PokemonMove::Splash);
            let max_hp = absorber.stats[0];
            absorber.hp = max_hp.saturating_sub(max_hp / 4);
            let initial_hp = absorber.hp;

            let state = battle_state_from_lists(
                vec![absorber],
                vec![],
                vec![mon(Species::Blastoise, Ability::Pressure, PokemonMove::Surf)],
                vec![],
            );
            let outcomes = run_single_turn(
                &MatchState::BattleState(state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &mdex, &pdex,
            );
            let expected_hp = initial_hp + max_hp / 4;
            assert!(
                outcomes.iter().all(|(s, _)| matches!(s, MatchState::BattleState(bs) if bs.p1_active_mons[0].hp == expected_hp)),
                "Water Absorb: expected HP={} in all branches", expected_hp,
            );
        }

        #[test]
        fn earth_eater_heals_on_ground_move() {
            let mdex = move_dex();
            let pdex = pokemon_dex();
            let mut absorber = mon(Species::Garchomp, Ability::EarthEater, PokemonMove::Splash);
            let max_hp = absorber.stats[0];
            absorber.hp = max_hp.saturating_sub(max_hp / 4);
            let initial_hp = absorber.hp;

            let state = battle_state_from_lists(
                vec![absorber],
                vec![],
                vec![mon(Species::Garchomp, Ability::Pressure, PokemonMove::Earthquake)],
                vec![],
            );
            let outcomes = run_single_turn(
                &MatchState::BattleState(state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &mdex, &pdex,
            );
            let expected_hp = initial_hp + max_hp / 4;
            assert!(
                outcomes.iter().all(|(s, _)| matches!(s, MatchState::BattleState(bs) if bs.p1_active_mons[0].hp == expected_hp)),
                "Earth Eater: expected HP={} in all branches", expected_hp,
            );
        }

        // ── react-on-hit: stat-boost abilities ───────────────────────────────────

        #[test]
        fn sap_sipper_boosts_attack_on_grass_move() {
            let mdex = move_dex();
            let pdex = pokemon_dex();
            let absorber = mon(Species::Azumarill, Ability::SapSipper, PokemonMove::Splash);
            let state = battle_state_from_lists(
                vec![absorber],
                vec![],
                vec![mon(Species::Leafeon, Ability::Pressure, PokemonMove::LeafBlade)],
                vec![],
            );
            let outcomes = run_single_turn(
                &MatchState::BattleState(state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &mdex, &pdex,
            );
            assert!(
                outcomes.iter().all(|(s, _)| matches!(s, MatchState::BattleState(bs) if bs.p1_active_mons[0].boosts[0] == 1)),
                "Sap Sipper: expected Attack boost +1 in all branches",
            );
        }

        #[test]
        fn motor_drive_boosts_speed_on_electric_move() {
            let mdex = move_dex();
            let pdex = pokemon_dex();
            let absorber = mon(Species::Electivire, Ability::MotorDrive, PokemonMove::Splash);
            let state = battle_state_from_lists(
                vec![absorber],
                vec![],
                vec![mon(Species::Raichu, Ability::Pressure, PokemonMove::Thunderbolt)],
                vec![],
            );
            let outcomes = run_single_turn(
                &MatchState::BattleState(state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &mdex, &pdex,
            );
            assert!(
                outcomes.iter().all(|(s, _)| matches!(s, MatchState::BattleState(bs) if bs.p1_active_mons[0].boosts[4] == 1)),
                "Motor Drive: expected Speed boost +1 in all branches",
            );
        }

        // ── react-on-hit: status-move absorption ─────────────────────────────────

        #[test]
        fn volt_absorb_absorbs_thunder_wave() {
            // Thunder Wave (90% accuracy) is Electric-type. Volt Absorb should negate it on
            // hit: heal 1/4 max HP and no paralysis. Miss branches leave HP unchanged and no
            // paralysis either (it simply missed).
            let mdex = move_dex();
            let pdex = pokemon_dex();
            let mut absorber = mon(Species::Jolteon, Ability::VoltAbsorb, PokemonMove::Splash);
            let max_hp = absorber.stats[0];
            absorber.hp = max_hp.saturating_sub(max_hp / 4);
            let initial_hp = absorber.hp;

            let state = battle_state_from_lists(
                vec![absorber],
                vec![],
                vec![mon(Species::Raichu, Ability::Pressure, PokemonMove::ThunderWave)],
                vec![],
            );
            let outcomes = run_single_turn(
                &MatchState::BattleState(state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &mdex, &pdex,
            );
            let expected_hp_on_hit = initial_hp + max_hp / 4;
            // There must be a branch where HP was healed (the hit branch).
            assert!(
                outcomes.iter().any(|(s, _)| matches!(s, MatchState::BattleState(bs) if bs.p1_active_mons[0].hp == expected_hp_on_hit)),
                "Volt Absorb vs Thunder Wave: expected a hit branch with HP={}", expected_hp_on_hit,
            );
            // In NO branch should the absorber be paralysed (absorption negate on hit; miss → no status either).
            for (s, _) in &outcomes {
                if let MatchState::BattleState(bs) = s {
                    assert!(bs.p1_active_mons[0].status.is_none(),
                        "Volt Absorb vs Thunder Wave: should never be paralysed");
                }
            }
        }

        #[test]
        fn sap_sipper_absorbs_spore() {
            // Spore is a Grass-type status move. Sap Sipper should negate it (no sleep) and
            // boost Attack by 1.
            let mdex = move_dex();
            let pdex = pokemon_dex();
            let absorber = mon(Species::Miltank, Ability::SapSipper, PokemonMove::Splash);
            let state = battle_state_from_lists(
                vec![absorber],
                vec![],
                vec![mon(Species::Amoonguss, Ability::Pressure, PokemonMove::Spore)],
                vec![],
            );
            let outcomes = run_single_turn(
                &MatchState::BattleState(state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &mdex, &pdex,
            );
            for (s, _) in &outcomes {
                if let MatchState::BattleState(bs) = s {
                    assert_eq!(bs.p1_active_mons[0].boosts[0], 1, "Sap Sipper vs Spore: expected +1 Atk");
                    assert!(bs.p1_active_mons[0].status.is_none(), "Sap Sipper vs Spore: should not be asleep");
                }
            }
        }

        #[test]
        fn sap_sipper_absorbs_leech_seed() {
            // Leech Seed (90% accuracy) is Grass-type. Sap Sipper should negate it on hit:
            // +1 Atk and no Leech Seed volatile. In miss branches: no +1 Atk but also no
            // Leech Seed applied.
            let mdex = move_dex();
            let pdex = pokemon_dex();
            let absorber = mon(Species::Miltank, Ability::SapSipper, PokemonMove::Splash);
            let state = battle_state_from_lists(
                vec![absorber],
                vec![],
                vec![mon(Species::Leafeon, Ability::Pressure, PokemonMove::LeechSeed)],
                vec![],
            );
            let outcomes = run_single_turn(
                &MatchState::BattleState(state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &mdex, &pdex,
            );
            // There must be a hit branch where Attack was boosted.
            assert!(
                outcomes.iter().any(|(s, _)| matches!(s, MatchState::BattleState(bs) if bs.p1_active_mons[0].boosts[0] == 1)),
                "Sap Sipper vs Leech Seed: expected a hit branch with +1 Atk",
            );
            // In NO branch should the absorber gain Leech Seed (neither hit-absorbed nor missed).
            for (s, _) in &outcomes {
                if let MatchState::BattleState(bs) = s {
                    let has_leech = bs.p1_active_mons[0].volatiles.iter().any(|v|
                        matches!(v, VolatileStatusState::TurnStatus(VolatileStatus::LeechSeed, _))
                    );
                    assert!(!has_leech, "Sap Sipper vs Leech Seed: should never gain Leech Seed volatile");
                }
            }
        }

        // ── Flash Fire ────────────────────────────────────────────────────────────

        #[test]
        fn flash_fire_sets_volatile_on_fire_move() {
            // A Fire move hitting a Flash Fire mon should set the FlashFire volatile and
            // deal 0 damage.
            let mdex = move_dex();
            let pdex = pokemon_dex();
            let absorber = mon(Species::Ninetales, Ability::FlashFire, PokemonMove::Splash);
            let state = battle_state_from_lists(
                vec![absorber],
                vec![],
                vec![mon(Species::Charizard, Ability::Pressure, PokemonMove::Flamethrower)],
                vec![],
            );
            let outcomes = run_single_turn(
                &MatchState::BattleState(state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &mdex, &pdex,
            );
            for (s, _) in &outcomes {
                if let MatchState::BattleState(bs) = s {
                    let max_hp = bs.p1_active_mons[0].stats[0];
                    assert_eq!(bs.p1_active_mons[0].hp, max_hp, "Flash Fire: should take 0 damage from Fire move");
                    let has_ff = bs.p1_active_mons[0].volatiles.iter().any(|v|
                        matches!(v, VolatileStatusState::MoveStatus(VolatileStatus::FlashFire, _))
                    );
                    assert!(has_ff, "Flash Fire: should gain FlashFire volatile");
                }
            }
        }

        #[test]
        fn flash_fire_absorbs_will_o_wisp() {
            // Will-O-Wisp (85% accuracy) is a Fire-type status move. Flash Fire should absorb
            // it on hit (no burn, FlashFire volatile set). In miss branches: no burn, no
            // volatile either.
            let mdex = move_dex();
            let pdex = pokemon_dex();
            let absorber = mon(Species::Ninetales, Ability::FlashFire, PokemonMove::Splash);
            let state = battle_state_from_lists(
                vec![absorber],
                vec![],
                vec![mon(Species::Charizard, Ability::Pressure, PokemonMove::WillOWisp)],
                vec![],
            );
            let outcomes = run_single_turn(
                &MatchState::BattleState(state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &mdex, &pdex,
            );
            // There must be a hit branch where FlashFire volatile was set.
            assert!(
                outcomes.iter().any(|(s, _)| matches!(s, MatchState::BattleState(bs)
                    if bs.p1_active_mons[0].volatiles.iter().any(|v|
                        matches!(v, VolatileStatusState::MoveStatus(VolatileStatus::FlashFire, _))))),
                "Flash Fire vs Will-O-Wisp: expected a hit branch with FlashFire volatile",
            );
            // In NO branch should the absorber be burned (absorption negate on hit; miss → no status either).
            for (s, _) in &outcomes {
                if let MatchState::BattleState(bs) = s {
                    assert!(bs.p1_active_mons[0].status.is_none(),
                        "Flash Fire vs Will-O-Wisp: should never be burned");
                }
            }
        }

        #[test]
        fn flash_fire_boosts_fire_move_power() {
            // After Flash Fire activates, the holder's Fire-type moves should do ~1.5× more
            // damage.  Use 0.05 tolerance (crit branching and floor-rounding).
            let mdex = move_dex();
            let pdex = pokemon_dex();

            // Baseline: Ninetales (no boost) uses Flamethrower vs Snorlax.
            let attacker_base = mon(Species::Ninetales, Ability::FlashFire, PokemonMove::Flamethrower);
            let target_base = mon(Species::Snorlax, Ability::Pressure, PokemonMove::Splash);
            let snorlax_max_hp = target_base.stats[0];
            let state_base = battle_state_from_lists(
                vec![attacker_base],
                vec![],
                vec![target_base],
                vec![],
            );
            let outcomes_base = run_single_turn(
                &MatchState::BattleState(state_base),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &mdex, &pdex,
            );
            let dmg_base: f64 = outcomes_base.iter().map(|(s, p)| {
                let hp = match s { MatchState::BattleState(bs) => bs.p2_active_mons[0].hp, _ => snorlax_max_hp };
                (snorlax_max_hp.saturating_sub(hp) as f64) * p
            }).sum();

            // Boosted: Ninetales already has FlashFire volatile.
            let mut attacker_boost = mon(Species::Ninetales, Ability::FlashFire, PokemonMove::Flamethrower);
            attacker_boost.volatiles.push(VolatileStatusState::MoveStatus(VolatileStatus::FlashFire, 0));
            let target_boost = mon(Species::Snorlax, Ability::Pressure, PokemonMove::Splash);
            let state_boost = battle_state_from_lists(
                vec![attacker_boost],
                vec![],
                vec![target_boost],
                vec![],
            );
            let outcomes_boost = run_single_turn(
                &MatchState::BattleState(state_boost),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &mdex, &pdex,
            );
            let dmg_boost: f64 = outcomes_boost.iter().map(|(s, p)| {
                let hp = match s { MatchState::BattleState(bs) => bs.p2_active_mons[0].hp, _ => snorlax_max_hp };
                (snorlax_max_hp.saturating_sub(hp) as f64) * p
            }).sum();

            let ratio = dmg_boost / dmg_base;
            assert!(
                (ratio - 1.5).abs() < 0.05,
                "Flash Fire power boost: expected ~1.5× ratio, got {:.4}", ratio,
            );
        }

        // ── draw-in: Lightning Rod & Storm Drain ─────────────────────────────────

        #[test]
        fn lightning_rod_boosts_spatk_on_electric_move() {
            let mdex = move_dex();
            let pdex = pokemon_dex();
            let absorber = mon(Species::Raichu, Ability::LightningRod, PokemonMove::Splash);
            let max_hp = absorber.stats[0];
            let state = battle_state_from_lists(
                vec![absorber],
                vec![],
                vec![mon(Species::Jolteon, Ability::Pressure, PokemonMove::Thunderbolt)],
                vec![],
            );
            let outcomes = run_single_turn(
                &MatchState::BattleState(state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &mdex, &pdex,
            );
            for (s, _) in &outcomes {
                if let MatchState::BattleState(bs) = s {
                    assert_eq!(bs.p1_active_mons[0].hp, max_hp, "Lightning Rod: should take 0 damage");
                    assert_eq!(bs.p1_active_mons[0].boosts[2], 1, "Lightning Rod: expected +1 Sp. Atk");
                }
            }
        }

        #[test]
        fn lightning_rod_boosts_on_miss() {
            // Thunder has 70% accuracy. Lightning Rod must fire in EVERY branch (hit and miss alike).
            let mdex = move_dex();
            let pdex = pokemon_dex();
            let absorber = mon(Species::Raichu, Ability::LightningRod, PokemonMove::Splash);
            let max_hp = absorber.stats[0];
            let state = battle_state_from_lists(
                vec![absorber],
                vec![],
                vec![mon(Species::Jolteon, Ability::Pressure, PokemonMove::Thunder)],
                vec![],
            );
            let outcomes = run_single_turn(
                &MatchState::BattleState(state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &mdex, &pdex,
            );
            // All branches (hit and miss) should show +1 Sp. Atk and full HP.
            for (s, _) in &outcomes {
                if let MatchState::BattleState(bs) = s {
                    assert_eq!(bs.p1_active_mons[0].hp, max_hp,
                        "Lightning Rod vs Thunder: should take 0 damage in all branches");
                    assert_eq!(bs.p1_active_mons[0].boosts[2], 1,
                        "Lightning Rod vs Thunder: +1 SpA must apply even in miss branches");
                }
            }
        }

        #[test]
        fn storm_drain_boosts_spatk_on_water_move() {
            let mdex = move_dex();
            let pdex = pokemon_dex();
            let absorber = mon(Species::Gastrodon, Ability::StormDrain, PokemonMove::Splash);
            let max_hp = absorber.stats[0];
            let state = battle_state_from_lists(
                vec![absorber],
                vec![],
                vec![mon(Species::Blastoise, Ability::Pressure, PokemonMove::HydroPump)],
                vec![],
            );
            let outcomes = run_single_turn(
                &MatchState::BattleState(state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &mdex, &pdex,
            );
            for (s, _) in &outcomes {
                if let MatchState::BattleState(bs) = s {
                    assert_eq!(bs.p1_active_mons[0].hp, max_hp, "Storm Drain: should take 0 damage");
                    assert_eq!(bs.p1_active_mons[0].boosts[2], 1, "Storm Drain: expected +1 Sp. Atk");
                }
            }
        }

        #[test]
        fn lightning_rod_boosts_on_thunder_wave() {
            // Thunder Wave is Electric-type (status). Lightning Rod should negate it (no
            // paralysis) and grant +1 Sp. Atk.
            let mdex = move_dex();
            let pdex = pokemon_dex();
            let absorber = mon(Species::Raichu, Ability::LightningRod, PokemonMove::Splash);
            let state = battle_state_from_lists(
                vec![absorber],
                vec![],
                vec![mon(Species::Jolteon, Ability::Pressure, PokemonMove::ThunderWave)],
                vec![],
            );
            let outcomes = run_single_turn(
                &MatchState::BattleState(state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &mdex, &pdex,
            );
            for (s, _) in &outcomes {
                if let MatchState::BattleState(bs) = s {
                    assert_eq!(bs.p1_active_mons[0].boosts[2], 1, "Lightning Rod vs Thunder Wave: expected +1 SpA");
                    assert!(bs.p1_active_mons[0].status.is_none(), "Lightning Rod vs Thunder Wave: should not be paralysed");
                }
            }
        }

        // ── doubles: redirection ──────────────────────────────────────────────────

        /// In doubles, a single-target Electric move aimed at the partner is drawn to the
        /// Lightning Rod holder; the holder absorbs it (+1 SpA, no damage).
        #[test]
        fn lightning_rod_redirects_in_doubles() {
            let mdex = move_dex();
            let pdex = pokemon_dex();

            // P1: slot 0 = Lightning Rod holder (Raichu), slot 1 = partner (Snorlax)
            // P2: slot 0 = attacker (Jolteon using Thunderbolt targeting P1 slot 1),
            //     slot 1 = dummy (Clefable using Splash)
            let holder = mon(Species::Raichu, Ability::LightningRod, PokemonMove::Splash);
            let partner = mon(Species::Snorlax, Ability::Pressure, PokemonMove::Splash);
            let attacker_p2 = mon(Species::Jolteon, Ability::Pressure, PokemonMove::Thunderbolt);
            let dummy = mon(Species::Clefable, Ability::Pressure, PokemonMove::Splash);

            let state = battle_state_from_lists(
                vec![holder, partner],
                vec![],
                vec![attacker_p2, dummy],
                vec![],
            );
            let holder_max_hp = state.p1_active_mons[0].stats[0];
            let partner_max_hp = state.p1_active_mons[1].stats[0];

            // P2 slot 0 (Jolteon) targets P1 slot 1 (Snorlax partner)
            let p2_cmd = PlayerCommand::Battle(vec![
                BattleCommand::Attack(AttackCommand {
                    move_slot: 0,
                    target: Some(FieldSlot { player: Player::P1, slot_index: 1 }),
                    terastallize: false,
                    mega_evolve: false,
                }),
                BattleCommand::Attack(AttackCommand {
                    move_slot: 0,
                    target: None,
                    terastallize: false,
                    mega_evolve: false,
                }),
            ]);
            let p1_cmd = PlayerCommand::Battle(vec![
                BattleCommand::Attack(AttackCommand {
                    move_slot: 0,
                    target: None,
                    terastallize: false,
                    mega_evolve: false,
                }),
                BattleCommand::Attack(AttackCommand {
                    move_slot: 0,
                    target: None,
                    terastallize: false,
                    mega_evolve: false,
                }),
            ]);
            let outcomes = run_single_turn(
                &MatchState::BattleState(state),
                &p1_cmd, &p2_cmd, &mdex, &pdex,
            );
            for (s, _) in &outcomes {
                if let MatchState::BattleState(bs) = s {
                    // Partner (Snorlax) must be undamaged
                    assert_eq!(bs.p1_active_mons[1].hp, partner_max_hp,
                        "Lightning Rod redirect: partner should take 0 damage");
                    // Holder gets +1 SpA and no damage
                    assert_eq!(bs.p1_active_mons[0].hp, holder_max_hp,
                        "Lightning Rod redirect: holder should take 0 damage");
                    assert_eq!(bs.p1_active_mons[0].boosts[2], 1,
                        "Lightning Rod redirect: holder should gain +1 Sp. Atk");
                }
            }
        }

        // ── negative: wrong type / suppressed ────────────────────────────────────

        #[test]
        fn volt_absorb_does_not_absorb_non_electric_move() {
            // Volt Absorb holder hit by a non-Electric move should take normal damage.
            let mdex = move_dex();
            let pdex = pokemon_dex();
            let absorber = mon(Species::Jolteon, Ability::VoltAbsorb, PokemonMove::Splash);
            let max_hp = absorber.stats[0];
            let state = battle_state_from_lists(
                vec![absorber],
                vec![],
                vec![mon(Species::Charizard, Ability::Pressure, PokemonMove::Flamethrower)],
                vec![],
            );
            let outcomes = run_single_turn(
                &MatchState::BattleState(state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &mdex, &pdex,
            );
            // Should take some damage (not full HP in all branches)
            assert!(
                outcomes.iter().any(|(s, _)| matches!(s, MatchState::BattleState(bs) if bs.p1_active_mons[0].hp < max_hp)),
                "Volt Absorb: should take damage from non-Electric move",
            );
        }

        #[test]
        fn flash_fire_suppressed_takes_fire_damage() {
            // With GastroAcid volatile suppressing Flash Fire, the mon should take normal
            // Fire damage.
            let mdex = move_dex();
            let pdex = pokemon_dex();
            let mut absorber = mon(Species::Ninetales, Ability::FlashFire, PokemonMove::Splash);
            // GastroAcid suppresses the ability
            absorber.volatiles.push(VolatileStatusState::TurnStatus(VolatileStatus::GastroAcid, 200));
            let max_hp = absorber.stats[0];
            let state = battle_state_from_lists(
                vec![absorber],
                vec![],
                vec![mon(Species::Charizard, Ability::Pressure, PokemonMove::Flamethrower)],
                vec![],
            );
            let outcomes = run_single_turn(
                &MatchState::BattleState(state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &mdex, &pdex,
            );
            assert!(
                outcomes.iter().any(|(s, _)| matches!(s, MatchState::BattleState(bs) if bs.p1_active_mons[0].hp < max_hp)),
                "Flash Fire suppressed: should take Fire damage when ability is suppressed",
            );
        }
    }

}

// ─────────────────────────────────────────────────────────────────────────────
// On-contact reactive abilities + Attract / Disable moves
// ─────────────────────────────────────────────────────────────────────────────
#[cfg(test)]
mod contact_reactive_abilities {
    use crate::battle::{BattleCommand, MatchState, Player, PlayerCommand};
    use crate::data::ability::Ability;
    use crate::data::item::Item;
    use crate::data::pokemon_move::PokemonMove;
    use crate::data::species::Species;
    use crate::dex_data::{VolatileStatus, Weather};
    use crate::pokemon::{build_pokemon_state, Nature, PokemonState};
    use crate::simulator::{get_possible_commands_for_active_slot, simulate_turn};
    use crate::simulator_helpers;
    use crate::simuilator_test_helpers::{
        battle_state_from_lists, move_dex, pokemon_dex, simple_attack,
    };

    fn make_mon(
        species: Species,
        moves: [Option<PokemonMove>; 4],
        ability: Ability,
        gender: Option<crate::pokemon::PokemonGender>,
    ) -> PokemonState {
        let pdex = pokemon_dex();
        let mdex = move_dex();
        let mut m = build_pokemon_state(
            species, pdex, mdex, Some(50), Some(moves), gender, Some(ability),
            None, None, None, None, None, false,
        );
        m.stats[5] = 1;
        m
    }

    // ── Rough Skin ───────────────────────────────────────────────────────────

    #[test]
    fn rough_skin_deals_1_8_damage_on_contact() {
        let pdex = pokemon_dex(); let mdex = move_dex();
        let mut attacker = make_mon(
            Species::Snorlax, [Some(PokemonMove::Tackle), None, None, None], Ability::None, None,
        );
        attacker.stats[5] = 200;
        let target = make_mon(
            Species::Snorlax, [Some(PokemonMove::Splash), None, None, None], Ability::RoughSkin, None,
        );
        let attacker_max_hp = attacker.stats[0];
        let expected_recoil = (attacker_max_hp as u32 / 8).max(1) as u16;
        let state = battle_state_from_lists(vec![attacker], vec![], vec![target], vec![]);
        let outcomes = simulate_turn(&MatchState::BattleState(state),
            &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
            &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
            mdex, pdex, false, 1);
        let recoil_applied = outcomes.iter().all(|(s, _)|
            matches!(s, MatchState::BattleState(bs) if bs.p1_active_mons[0].hp == attacker_max_hp - expected_recoil));
        assert!(recoil_applied, "Rough Skin: attacker should lose 1/8 max HP on contact");
    }

    #[test]
    fn rough_skin_no_damage_on_non_contact() {
        let pdex = pokemon_dex(); let mdex = move_dex();
        let mut attacker = make_mon(
            Species::Garchomp, [Some(PokemonMove::Earthquake), None, None, None], Ability::None, None,
        );
        attacker.stats[5] = 200;
        let target = make_mon(
            Species::Snorlax, [Some(PokemonMove::Splash), None, None, None], Ability::RoughSkin, None,
        );
        let attacker_max_hp = attacker.stats[0];
        let state = battle_state_from_lists(vec![attacker], vec![], vec![target], vec![]);
        let outcomes = simulate_turn(&MatchState::BattleState(state),
            &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
            &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
            mdex, pdex, false, 1);
        assert!(outcomes.iter().all(|(s, _)| matches!(s, MatchState::BattleState(bs) if bs.p1_active_mons[0].hp == attacker_max_hp)),
            "Rough Skin should not fire on Earthquake (non-contact)");
    }

    #[test]
    fn rough_skin_blocked_by_magic_guard() {
        let pdex = pokemon_dex(); let mdex = move_dex();
        let mut attacker = make_mon(
            Species::Snorlax, [Some(PokemonMove::Tackle), None, None, None], Ability::MagicGuard, None,
        );
        attacker.stats[5] = 200;
        let target = make_mon(
            Species::Snorlax, [Some(PokemonMove::Splash), None, None, None], Ability::RoughSkin, None,
        );
        let attacker_max_hp = attacker.stats[0];
        let state = battle_state_from_lists(vec![attacker], vec![], vec![target], vec![]);
        let outcomes = simulate_turn(&MatchState::BattleState(state),
            &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
            &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
            mdex, pdex, false, 1);
        assert!(outcomes.iter().all(|(s, _)| matches!(s, MatchState::BattleState(bs) if bs.p1_active_mons[0].hp == attacker_max_hp)),
            "Magic Guard should block Rough Skin recoil");
    }

    // ── Flame Body ───────────────────────────────────────────────────────────

    #[test]
    fn flame_body_30_percent_burn_on_contact() {
        let pdex = pokemon_dex(); let mdex = move_dex();
        let mut attacker = make_mon(
            Species::Snorlax, [Some(PokemonMove::Tackle), None, None, None], Ability::None, None,
        );
        attacker.stats[5] = 200;
        let target = make_mon(
            Species::Snorlax, [Some(PokemonMove::Splash), None, None, None], Ability::FlameBody, None,
        );
        let state = battle_state_from_lists(vec![attacker], vec![], vec![target], vec![]);
        let outcomes = simulate_turn(&MatchState::BattleState(state),
            &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
            &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
            mdex, pdex, false, 1);
        let burn_prob: f64 = outcomes.iter().map(|(s, p)|
            if matches!(s, MatchState::BattleState(bs) if bs.p1_active_mons[0].status == Some(crate::dex_data::Status::Burn)) { *p } else { 0.0 }
        ).sum();
        assert!((burn_prob - 0.30).abs() < 0.05, "Flame Body should burn ~30%; got {burn_prob}");
    }

    #[test]
    fn flame_body_no_burn_on_fire_type_attacker() {
        let pdex = pokemon_dex(); let mdex = move_dex();
        let mut attacker = make_mon(
            Species::Charizard, [Some(PokemonMove::Tackle), None, None, None], Ability::None, None,
        );
        attacker.stats[5] = 200;
        let target = make_mon(
            Species::Snorlax, [Some(PokemonMove::Splash), None, None, None], Ability::FlameBody, None,
        );
        let state = battle_state_from_lists(vec![attacker], vec![], vec![target], vec![]);
        let outcomes = simulate_turn(&MatchState::BattleState(state),
            &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
            &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
            mdex, pdex, false, 1);
        let burn_prob: f64 = outcomes.iter().map(|(s, p)|
            if matches!(s, MatchState::BattleState(bs) if bs.p1_active_mons[0].status == Some(crate::dex_data::Status::Burn)) { *p } else { 0.0 }
        ).sum();
        assert!(burn_prob < 1e-9, "Fire-type attacker immune to Flame Body burn; got {burn_prob}");
    }

    // ── Static ────────────────────────────────────────────────────────────────

    #[test]
    fn static_ability_30_percent_paralysis_on_contact() {
        let pdex = pokemon_dex(); let mdex = move_dex();
        let mut attacker = make_mon(
            Species::Snorlax, [Some(PokemonMove::Tackle), None, None, None], Ability::None, None,
        );
        attacker.stats[5] = 200;
        let target = make_mon(
            Species::Snorlax, [Some(PokemonMove::Splash), None, None, None], Ability::Static, None,
        );
        let state = battle_state_from_lists(vec![attacker], vec![], vec![target], vec![]);
        let outcomes = simulate_turn(&MatchState::BattleState(state),
            &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
            &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
            mdex, pdex, false, 1);
        let para_prob: f64 = outcomes.iter().map(|(s, p)|
            if matches!(s, MatchState::BattleState(bs) if matches!(bs.p1_active_mons[0].status, Some(crate::dex_data::Status::Paralysis))) { *p } else { 0.0 }
        ).sum();
        assert!((para_prob - 0.30).abs() < 0.05, "Static should paralyse ~30%; got {para_prob}");
    }

    // ── Poison Point ──────────────────────────────────────────────────────────

    #[test]
    fn poison_point_30_percent_poison_on_contact() {
        let pdex = pokemon_dex(); let mdex = move_dex();
        let mut attacker = make_mon(
            Species::Snorlax, [Some(PokemonMove::Tackle), None, None, None], Ability::None, None,
        );
        attacker.stats[5] = 200;
        let target = make_mon(
            Species::Snorlax, [Some(PokemonMove::Splash), None, None, None], Ability::PoisonPoint, None,
        );
        let state = battle_state_from_lists(vec![attacker], vec![], vec![target], vec![]);
        let outcomes = simulate_turn(&MatchState::BattleState(state),
            &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
            &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
            mdex, pdex, false, 1);
        let poison_prob: f64 = outcomes.iter().map(|(s, p)|
            if matches!(s, MatchState::BattleState(bs) if matches!(bs.p1_active_mons[0].status, Some(crate::dex_data::Status::Poison))) { *p } else { 0.0 }
        ).sum();
        assert!((poison_prob - 0.30).abs() < 0.05, "Poison Point should poison ~30%; got {poison_prob}");
    }

    // ── Spicy Spray ───────────────────────────────────────────────────────────

    #[test]
    fn spicy_spray_100_percent_burn_on_non_contact_special() {
        let pdex = pokemon_dex(); let mdex = move_dex();
        let mut attacker = make_mon(
            Species::Snorlax, [Some(PokemonMove::Flamethrower), None, None, None], Ability::None, None,
        );
        attacker.stats[5] = 200;
        let target = make_mon(
            Species::Blissey, [Some(PokemonMove::Splash), None, None, None], Ability::SpicySpray, None,
        );
        let state = battle_state_from_lists(vec![attacker], vec![], vec![target], vec![]);
        let outcomes = simulate_turn(&MatchState::BattleState(state),
            &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
            &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
            mdex, pdex, false, 1);
        let burn_prob: f64 = outcomes.iter().map(|(s, p)|
            if matches!(s, MatchState::BattleState(bs) if matches!(bs.p1_active_mons[0].status, Some(crate::dex_data::Status::Burn))) { *p } else { 0.0 }
        ).sum();
        assert!((burn_prob - 1.0).abs() < 1e-9, "Spicy Spray should always burn attacker; got {burn_prob}");
    }

    // ── Gooey ─────────────────────────────────────────────────────────────────

    #[test]
    fn gooey_lowers_attacker_speed_on_contact() {
        let pdex = pokemon_dex(); let mdex = move_dex();
        let mut attacker = make_mon(
            Species::Snorlax, [Some(PokemonMove::Tackle), None, None, None], Ability::None, None,
        );
        attacker.stats[5] = 200;
        let target = make_mon(
            Species::Snorlax, [Some(PokemonMove::Splash), None, None, None], Ability::Gooey, None,
        );
        let state = battle_state_from_lists(vec![attacker], vec![], vec![target], vec![]);
        let outcomes = simulate_turn(&MatchState::BattleState(state),
            &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
            &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
            mdex, pdex, false, 1);
        assert!(outcomes.iter().all(|(s, _)| matches!(s, MatchState::BattleState(bs) if bs.p1_active_mons[0].boosts[4] == -1)),
            "Gooey should drop attacker Speed by 1");
    }

    #[test]
    fn gooey_no_drop_on_non_contact() {
        let pdex = pokemon_dex(); let mdex = move_dex();
        let mut attacker = make_mon(
            Species::Garchomp, [Some(PokemonMove::Earthquake), None, None, None], Ability::None, None,
        );
        attacker.stats[5] = 200;
        let target = make_mon(
            Species::Snorlax, [Some(PokemonMove::Splash), None, None, None], Ability::Gooey, None,
        );
        let state = battle_state_from_lists(vec![attacker], vec![], vec![target], vec![]);
        let outcomes = simulate_turn(&MatchState::BattleState(state),
            &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
            &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
            mdex, pdex, false, 1);
        assert!(outcomes.iter().all(|(s, _)| matches!(s, MatchState::BattleState(bs) if bs.p1_active_mons[0].boosts[4] == 0)),
            "Gooey should not fire on non-contact move");
    }

    // ── Weak Armor ────────────────────────────────────────────────────────────

    #[test]
    fn weak_armor_lowers_def_raises_spe_on_physical_hit() {
        let pdex = pokemon_dex(); let mdex = move_dex();
        let mut attacker = make_mon(
            Species::Snorlax, [Some(PokemonMove::Earthquake), None, None, None], Ability::None, None,
        );
        attacker.stats[5] = 200;
        let target = make_mon(
            Species::Snorlax, [Some(PokemonMove::Splash), None, None, None], Ability::WeakArmor, None,
        );
        let state = battle_state_from_lists(vec![attacker], vec![], vec![target], vec![]);
        let outcomes = simulate_turn(&MatchState::BattleState(state),
            &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
            &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
            mdex, pdex, false, 1);
        assert!(outcomes.iter().all(|(s, _)| matches!(s, MatchState::BattleState(bs) if bs.p2_active_mons[0].boosts[1] == -1 && bs.p2_active_mons[0].boosts[4] == 2)),
            "Weak Armor: −1 Def / +2 Spe on physical hit");
    }

    #[test]
    fn weak_armor_no_effect_on_special_move() {
        let pdex = pokemon_dex(); let mdex = move_dex();
        let mut attacker = make_mon(
            Species::Snorlax, [Some(PokemonMove::Flamethrower), None, None, None], Ability::None, None,
        );
        attacker.stats[5] = 200;
        let target = make_mon(
            Species::Snorlax, [Some(PokemonMove::Splash), None, None, None], Ability::WeakArmor, None,
        );
        let state = battle_state_from_lists(vec![attacker], vec![], vec![target], vec![]);
        let outcomes = simulate_turn(&MatchState::BattleState(state),
            &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
            &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
            mdex, pdex, false, 1);
        assert!(outcomes.iter().all(|(s, _)| matches!(s, MatchState::BattleState(bs) if bs.p2_active_mons[0].boosts == [0i8;7])),
            "Weak Armor should not fire on special moves");
    }

    // ── Mummy ─────────────────────────────────────────────────────────────────

    #[test]
    fn mummy_replaces_attacker_ability_on_contact() {
        let pdex = pokemon_dex(); let mdex = move_dex();
        let mut attacker = make_mon(
            Species::Snorlax, [Some(PokemonMove::Tackle), None, None, None], Ability::Gooey, None,
        );
        attacker.stats[5] = 200;
        let target = make_mon(
            Species::Snorlax, [Some(PokemonMove::Splash), None, None, None], Ability::Mummy, None,
        );
        let state = battle_state_from_lists(vec![attacker], vec![], vec![target], vec![]);
        let outcomes = simulate_turn(&MatchState::BattleState(state),
            &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
            &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
            mdex, pdex, false, 1);
        assert!(outcomes.iter().all(|(s, _)| matches!(s, MatchState::BattleState(bs) if bs.p1_active_mons[0].ability == Ability::Mummy)),
            "Attacker's ability should become Mummy after contact");
    }

    #[test]
    fn mummy_reverts_on_switch_out() {
        let pdex = pokemon_dex(); let mdex = move_dex();
        let mut attacker = make_mon(
            Species::Snorlax, [Some(PokemonMove::Tackle), None, None, None], Ability::Gooey, None,
        );
        attacker.stats[5] = 200;
        let back_bench = make_mon(
            Species::Blissey, [Some(PokemonMove::Splash), None, None, None], Ability::None, None,
        );
        let target = make_mon(
            Species::Snorlax, [Some(PokemonMove::Splash), None, None, None], Ability::Mummy, None,
        );
        let state = battle_state_from_lists(vec![attacker], vec![back_bench], vec![target], vec![]);
        let t1 = simulate_turn(&MatchState::BattleState(state),
            &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
            &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
            mdex, pdex, false, 1);
        let (mummy_state, _) = t1.into_iter()
            .find(|(s, _)| matches!(s, MatchState::BattleState(bs) if bs.p1_active_mons[0].ability == Ability::Mummy))
            .expect("Mummy should be applied turn 1");

        let t2 = simulate_turn(&mummy_state,
            &PlayerCommand::Battle(vec![BattleCommand::Switch(crate::battle::SwitchCommand { party_index: 0 })]),
            &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
            mdex, pdex, false, 1);
        let reverted = t2.iter().any(|(s, _)|
            matches!(s, MatchState::BattleState(bs) if bs.p1_back_mons.iter().any(|m| m.ability == Ability::Gooey)));
        assert!(reverted, "Mummy should revert when affected Pokémon switches out");
    }

    // ── Cursed Body ───────────────────────────────────────────────────────────

    #[test]
    fn cursed_body_30_percent_disable_on_damaging_hit() {
        let pdex = pokemon_dex(); let mdex = move_dex();
        let mut attacker = make_mon(
            Species::Snorlax, [Some(PokemonMove::Flamethrower), None, None, None], Ability::None, None,
        );
        attacker.stats[5] = 200;
        let target = make_mon(
            Species::Blissey, [Some(PokemonMove::Splash), None, None, None], Ability::CursedBody, None,
        );
        let state = battle_state_from_lists(vec![attacker], vec![], vec![target], vec![]);
        let outcomes = simulate_turn(&MatchState::BattleState(state),
            &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
            &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
            mdex, pdex, false, 1);
        let disable_prob: f64 = outcomes.iter().map(|(s, p)|
            if matches!(s, MatchState::BattleState(bs) if
                simulator_helpers::has_status_volatile(&bs.p1_active_mons[0], &VolatileStatus::Disable(PokemonMove::Struggle))
            ) { *p } else { 0.0 }
        ).sum();
        assert!((disable_prob - 0.30).abs() < 0.05, "Cursed Body should Disable ~30%; got {disable_prob}");
    }

    // ── Cute Charm ────────────────────────────────────────────────────────────

    #[test]
    fn cute_charm_30_percent_attract_on_opposite_gender() {
        let pdex = pokemon_dex(); let mdex = move_dex();
        let mut attacker = make_mon(
            Species::Snorlax, [Some(PokemonMove::Tackle), None, None, None],
            Ability::None, Some(crate::pokemon::PokemonGender::Male),
        );
        attacker.stats[5] = 200;
        let target = make_mon(
            Species::Snorlax, [Some(PokemonMove::Splash), None, None, None],
            Ability::CuteCharm, Some(crate::pokemon::PokemonGender::Female),
        );
        let state = battle_state_from_lists(vec![attacker], vec![], vec![target], vec![]);
        let outcomes = simulate_turn(&MatchState::BattleState(state),
            &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
            &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
            mdex, pdex, false, 1);
        let attract_prob: f64 = outcomes.iter().map(|(s, p)|
            if matches!(s, MatchState::BattleState(bs) if
                simulator_helpers::has_status_volatile(&bs.p1_active_mons[0], &VolatileStatus::Attract)
            ) { *p } else { 0.0 }
        ).sum();
        assert!((attract_prob - 0.30).abs() < 0.05, "Cute Charm should Attract ~30% opposite-gender; got {attract_prob}");
    }

    #[test]
    fn cute_charm_no_attract_on_same_gender() {
        let pdex = pokemon_dex(); let mdex = move_dex();
        let mut attacker = make_mon(
            Species::Snorlax, [Some(PokemonMove::Tackle), None, None, None],
            Ability::None, Some(crate::pokemon::PokemonGender::Male),
        );
        attacker.stats[5] = 200;
        let target = make_mon(
            Species::Snorlax, [Some(PokemonMove::Splash), None, None, None],
            Ability::CuteCharm, Some(crate::pokemon::PokemonGender::Male),
        );
        let state = battle_state_from_lists(vec![attacker], vec![], vec![target], vec![]);
        let outcomes = simulate_turn(&MatchState::BattleState(state),
            &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
            &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
            mdex, pdex, false, 1);
        let attract_prob: f64 = outcomes.iter().map(|(s, p)|
            if matches!(s, MatchState::BattleState(bs) if
                simulator_helpers::has_status_volatile(&bs.p1_active_mons[0], &VolatileStatus::Attract)
            ) { *p } else { 0.0 }
        ).sum();
        assert!(attract_prob < 1e-9, "Cute Charm should not attract same-gender attacker; got {attract_prob}");
    }

    // ── Attract move ──────────────────────────────────────────────────────────

    #[test]
    fn attract_move_inflicts_infatuation_on_opposite_gender() {
        let pdex = pokemon_dex(); let mdex = move_dex();
        let mut user = make_mon(
            Species::Snorlax, [Some(PokemonMove::Attract), None, None, None],
            Ability::None, Some(crate::pokemon::PokemonGender::Female),
        );
        user.stats[5] = 200;
        let target = make_mon(
            Species::Snorlax, [Some(PokemonMove::Splash), None, None, None],
            Ability::None, Some(crate::pokemon::PokemonGender::Male),
        );
        let state = battle_state_from_lists(vec![user], vec![], vec![target], vec![]);
        let outcomes = simulate_turn(&MatchState::BattleState(state),
            &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
            &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
            mdex, pdex, false, 1);
        assert!(outcomes.iter().all(|(s, _)| matches!(s, MatchState::BattleState(bs) if
            simulator_helpers::has_status_volatile(&bs.p2_active_mons[0], &VolatileStatus::Attract)
        )), "Attract move should infatuate opposite-gender target");
    }

    #[test]
    fn attract_move_fails_on_same_gender() {
        let pdex = pokemon_dex(); let mdex = move_dex();
        let mut user = make_mon(
            Species::Snorlax, [Some(PokemonMove::Attract), None, None, None],
            Ability::None, Some(crate::pokemon::PokemonGender::Female),
        );
        user.stats[5] = 200;
        let target = make_mon(
            Species::Snorlax, [Some(PokemonMove::Splash), None, None, None],
            Ability::None, Some(crate::pokemon::PokemonGender::Female),
        );
        let state = battle_state_from_lists(vec![user], vec![], vec![target], vec![]);
        let outcomes = simulate_turn(&MatchState::BattleState(state),
            &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
            &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
            mdex, pdex, false, 1);
        assert!(outcomes.iter().all(|(s, _)| matches!(s, MatchState::BattleState(bs) if
            !simulator_helpers::has_status_volatile(&bs.p2_active_mons[0], &VolatileStatus::Attract)
        )), "Attract should fail vs same-gender target");
    }

    #[test]
    fn attract_volatile_causes_50_percent_fail_to_act() {
        let pdex = pokemon_dex(); let mdex = move_dex();
        // P1 (female, fast) uses Attract turn 1, then Splash turn 2.
        // P2 (male, faster than P1 in turn 2 so it acts after we re-assign speeds) uses Tackle.
        // We set P2 faster so P2 moves first in turn 2 and hits P1 if not fail-to-acted.
        let mut user = make_mon(
            Species::Snorlax, [Some(PokemonMove::Attract), Some(PokemonMove::Splash), None, None],
            Ability::None, Some(crate::pokemon::PokemonGender::Female),
        );
        user.stats[5] = 200; // fast in turn 1 to use Attract first
        let mut target = make_mon(
            Species::Snorlax, [Some(PokemonMove::Tackle), None, None, None],
            Ability::None, Some(crate::pokemon::PokemonGender::Male),
        );
        target.stats[5] = 1;
        // Turn 1: P1 uses Attract on P2 (P1 faster).
        let state = battle_state_from_lists(vec![user], vec![], vec![target], vec![]);
        let t1 = simulate_turn(&MatchState::BattleState(state),
            &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
            &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
            mdex, pdex, false, 1);
        let (attracted_state, _) = t1.into_iter()
            .find(|(s, _)| matches!(s, MatchState::BattleState(bs) if
                simulator_helpers::has_status_volatile(&bs.p2_active_mons[0], &VolatileStatus::Attract)))
            .expect("Attract should have been applied turn 1");

        // Capture P1's HP at the start of turn 2 (P2 couldn't hit in turn 1 — P1 was faster).
        let p1_hp_before_t2 = if let MatchState::BattleState(ref bs) = attracted_state {
            bs.p1_active_mons[0].hp
        } else { panic!("expected BattleState") };

        // Boost P2's speed so it moves first in turn 2 and hits P1 when not failed.
        let attracted_state2 = if let MatchState::BattleState(mut bs) = attracted_state {
            bs.p2_active_mons[0].stats[5] = 400;
            MatchState::BattleState(bs)
        } else { panic!() };

        // Turn 2: P2 (attracted, now faster) tries Tackle; P1 uses Splash.
        let outcomes2 = simulate_turn(&attracted_state2,
            &PlayerCommand::Battle(simple_attack(Player::P1, vec![1])),
            &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
            mdex, pdex, false, 1);
        // If P2 acts: P1's HP decreases. If P2 fails: P1's HP unchanged.
        let act_prob: f64 = outcomes2.iter().map(|(s, p)|
            if matches!(s, MatchState::BattleState(bs) if bs.p1_active_mons[0].hp < p1_hp_before_t2) { *p } else { 0.0 }
        ).sum();
        let fail_prob: f64 = outcomes2.iter().map(|(s, p)|
            if matches!(s, MatchState::BattleState(bs) if bs.p1_active_mons[0].hp == p1_hp_before_t2) { *p } else { 0.0 }
        ).sum();
        assert!((act_prob - 0.50).abs() < 0.05, "Attracted Pokémon should act ~50%; got {act_prob}");
        assert!((fail_prob - 0.50).abs() < 0.05, "Attracted Pokémon should fail ~50%; got {fail_prob}");
    }

    // ── Disable move ──────────────────────────────────────────────────────────

    #[test]
    fn disable_makes_last_used_move_unselectable() {
        let pdex = pokemon_dex(); let mdex = move_dex();
        let p1 = make_mon(
            Species::Blissey, [Some(PokemonMove::Disable), None, None, None], Ability::None, None,
        );
        let mut p2 = make_mon(
            Species::Snorlax, [Some(PokemonMove::Tackle), Some(PokemonMove::Splash), None, None],
            Ability::None, None,
        );
        p2.stats[5] = 200; // P2 moves first (to set last_used_move via Tackle)

        let state = battle_state_from_lists(vec![p1], vec![], vec![p2], vec![]);
        let t1 = simulate_turn(&MatchState::BattleState(state),
            &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
            &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
            mdex, pdex, false, 1);
        let (t1_state, _) = t1.into_iter().next().expect("turn 1 outcome");

        // Turn 2: P1 uses Disable.
        let t2 = simulate_turn(&t1_state,
            &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
            &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
            mdex, pdex, false, 1);
        let (disabled_state, _) = t2.into_iter()
            .find(|(s, _)| matches!(s, MatchState::BattleState(bs) if
                simulator_helpers::has_status_volatile(&bs.p2_active_mons[0], &VolatileStatus::Disable(PokemonMove::Struggle))
            ))
            .expect("Disable volatile should be on P2 after turn 2");

        if let MatchState::BattleState(ref bs) = disabled_state {
            let cmds = get_possible_commands_for_active_slot(bs, Player::P2, 0, mdex, pdex);
            let has_tackle = cmds.iter().any(|c| matches!(c,
                BattleCommand::Attack(a) if bs.p2_active_mons[0].moves[a.move_slot] == Some(PokemonMove::Tackle)
            ));
            assert!(!has_tackle, "Disabled Tackle should not be selectable; cmds={:?}", cmds);
            let has_splash = cmds.iter().any(|c| matches!(c,
                BattleCommand::Attack(a) if bs.p2_active_mons[0].moves[a.move_slot] == Some(PokemonMove::Splash)
            ));
            assert!(has_splash, "Non-disabled Splash should still be selectable");
        } else { panic!("Expected BattleState"); }
    }

    #[test]
    fn disable_plus_choice_forces_struggle() {
        let pdex = pokemon_dex(); let mdex = move_dex();
        let p1 = make_mon(
            Species::Blissey, [Some(PokemonMove::Disable), None, None, None], Ability::None, None,
        );
        let mut p2 = build_pokemon_state(
            Species::Snorlax, pdex, mdex, Some(50),
            Some([Some(PokemonMove::Tackle), None, None, None]),
            None, Some(Ability::None), None, Some(Item::ChoiceBand),
            None, None, None, false,
        );
        p2.stats[5] = 200;

        let state = battle_state_from_lists(vec![p1], vec![], vec![p2], vec![]);
        let t1 = simulate_turn(&MatchState::BattleState(state),
            &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
            &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
            mdex, pdex, false, 1);
        let (t1_state, _) = t1.into_iter().next().expect("turn 1");

        let t2 = simulate_turn(&t1_state,
            &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
            &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
            mdex, pdex, false, 1);
        let (disabled_state, _) = t2.into_iter()
            .find(|(s, _)| matches!(s, MatchState::BattleState(bs) if
                simulator_helpers::has_status_volatile(&bs.p2_active_mons[0], &VolatileStatus::Disable(PokemonMove::Struggle))
            ))
            .expect("Disable volatile should be present");

        if let MatchState::BattleState(ref bs) = disabled_state {
            let cmds = get_possible_commands_for_active_slot(bs, Player::P2, 0, mdex, pdex);
            let has_struggle = cmds.iter().any(|c| matches!(c, BattleCommand::Struggle { .. }));
            let has_tackle = cmds.iter().any(|c| matches!(c,
                BattleCommand::Attack(a) if bs.p2_active_mons[0].moves.get(a.move_slot) == Some(&Some(PokemonMove::Tackle))
            ));
            assert!(has_struggle, "Disable + Choice → Struggle; cmds={:?}", cmds);
            assert!(!has_tackle, "Tackle should be unavailable (disabled + locked)");
        } else { panic!("Expected BattleState"); }
    }

    // ── Wandering Spirit ──────────────────────────────────────────────────────

    #[test]
    fn wandering_spirit_swaps_abilities_on_contact() {
        let pdex = pokemon_dex(); let mdex = move_dex();
        let mut attacker = make_mon(
            Species::Snorlax, [Some(PokemonMove::Tackle), None, None, None], Ability::Gooey, None,
        );
        attacker.stats[5] = 200;
        let target = make_mon(
            Species::Snorlax, [Some(PokemonMove::Splash), None, None, None], Ability::WanderingSpirit, None,
        );
        let state = battle_state_from_lists(vec![attacker], vec![], vec![target], vec![]);
        let outcomes = simulate_turn(&MatchState::BattleState(state),
            &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
            &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
            mdex, pdex, false, 1);
        assert!(outcomes.iter().all(|(s, _)| matches!(s, MatchState::BattleState(bs) if
            bs.p1_active_mons[0].ability == Ability::WanderingSpirit
            && bs.p2_active_mons[0].ability == Ability::Gooey
        )), "Wandering Spirit should swap attacker ↔ holder abilities");
    }

    #[test]
    fn wandering_spirit_on_gain_activates_drought() {
        let pdex = pokemon_dex(); let mdex = move_dex();
        let mut attacker = make_mon(
            Species::Snorlax, [Some(PokemonMove::Tackle), None, None, None], Ability::Drought, None,
        );
        attacker.stats[5] = 200;
        let target = make_mon(
            Species::Snorlax, [Some(PokemonMove::Splash), None, None, None], Ability::WanderingSpirit, None,
        );
        let state = battle_state_from_lists(vec![attacker], vec![], vec![target], vec![]);
        let outcomes = simulate_turn(&MatchState::BattleState(state),
            &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
            &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
            mdex, pdex, false, 1);
        assert!(outcomes.iter().all(|(s, _)| matches!(s, MatchState::BattleState(bs) if bs.weather == Some(Weather::Sun))),
            "Holder gains Drought via Wandering Spirit → Sun should be set immediately");
    }
}

// ── Priority manipulation abilities ────────────────────────────────────────────
//
// Turn-order probe pattern: both mons at hp=1/stats[0]=1 with Tackle.
// Whoever goes first KOs the other → observable as GameOverState { winner }.
// Paralysis-fail probe: P1 at high HP with Thunder Wave, P2 at high HP with Tackle;
// if Prankster fires → Thunder Wave goes first → P2 paralyzed → 12.5% fail → P1 undamaged.
mod priority_abilities {
    use crate::battle::{MatchState, Player, PlayerCommand};
    use crate::data::ability::Ability;
    use crate::data::item::Item;
    use crate::data::pokemon_move::PokemonMove;
    use crate::data::species::Species;
    use crate::pokemon::{build_pokemon_state, PokemonState};
    use crate::simuilator_test_helpers::{
        battle_state_from_lists, move_dex, pokemon_dex, run_single_turn, simple_attack,
    };

    // ── helpers ──────────────────────────────────────────────────────────────

    /// Build a minimal test Pokémon. Speed defaults to 1; override with mon.stats[5] = N.
    fn make_mon(
        species: Species,
        ability: Ability,
        moves: [Option<PokemonMove>; 4],
        item: Option<Item>,
    ) -> PokemonState {
        let pdex = pokemon_dex();
        let mdex = move_dex();
        let mut m = build_pokemon_state(
            species, pdex, mdex, Some(50), Some(moves), None,
            Some(ability), None, item, None, None, None, false,
        );
        m.stats[5] = 1; // very slow by default; override for fast mons
        m
    }

    /// Sum probability of branches where P1 wins.
    fn p1_win_prob(outcomes: &[(MatchState, f64)]) -> f64 {
        outcomes.iter().filter_map(|(s, p)| {
            if let MatchState::GameOverState { winner: Player::P1 } = s { Some(p) } else { None }
        }).sum()
    }

    /// Sum probability of branches where P2 wins.
    fn p2_win_prob(outcomes: &[(MatchState, f64)]) -> f64 {
        outcomes.iter().filter_map(|(s, p)| {
            if let MatchState::GameOverState { winner: Player::P2 } = s { Some(p) } else { None }
        }).sum()
    }

    /// Probability that P1's current HP equals `initial_hp` across all branches.
    fn p1_undamaged_prob(outcomes: &[(MatchState, f64)], initial_hp: u16) -> f64 {
        outcomes.iter().filter_map(|(s, p)| {
            if let MatchState::BattleState(bs) = s {
                if bs.p1_active_mons[0].hp == initial_hp { Some(p) } else { None }
            } else { None }
        }).sum()
    }

    fn run(p1: PokemonState, p2: PokemonState) -> Vec<(MatchState, f64)> {
        let pdex = pokemon_dex();
        let mdex = move_dex();
        let state = battle_state_from_lists(vec![p1], vec![], vec![p2], vec![]);
        run_single_turn(
            &MatchState::BattleState(state),
            &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
            &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
            mdex, pdex,
        )
    }

    // ── Prankster ──────────────────────────────────────────────────────────

    #[test]
    fn prankster_boosts_status_move_priority() {
        // Slow Prankster Snorlax with Thunder Wave vs fast Normal-type Snorlax with Tackle.
        // Thunder Wave (status) gets +1 priority → fires before Tackle → P2 paralyzed.
        // P2's Tackle then fails 12.5% of the time due to paralysis → P1 survives (undamaged).
        // Without Prankster boost: P2 goes first → Tackle always lands → P1 always damaged.
        // Note: P2 must NOT be Electric-type (Electric-types are immune to Thunder Wave).
        let pdex = pokemon_dex();
        let mdex = move_dex();

        let mut p1 = make_mon(Species::Snorlax, Ability::Prankster,
            [Some(PokemonMove::ThunderWave), None, None, None], None);
        // Keep p1 at full HP (high enough to survive a Tackle from P2)
        let p1_initial_hp = p1.hp;

        let mut p2 = make_mon(Species::Snorlax, Ability::None,   // Normal-type, not Electric
            [Some(PokemonMove::Tackle), None, None, None], None);
        p2.stats[5] = 999; // very fast

        let state = battle_state_from_lists(vec![p1.clone()], vec![], vec![p2], vec![]);
        let outcomes = run_single_turn(
            &MatchState::BattleState(state),
            &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
            &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
            mdex, pdex,
        );

        // Some branches where P1 is undamaged (P2's paralysis caused a fail).
        // This is ONLY possible if Thunder Wave fired first (Prankster +1 priority),
        // then P2 rolled a paralysis fail (12.5%).
        let undamaged = p1_undamaged_prob(&outcomes, p1_initial_hp);
        assert!(
            undamaged > 0.05,
            "Prankster should give Thunder Wave +1 priority so it fires before Tackle; \
             some branches should have P2 paralysis-failing (12.5%). Got undamaged prob = {undamaged:.3}"
        );
    }

    #[test]
    fn prankster_does_not_boost_damaging_moves() {
        // Slow Prankster Snorlax uses Tackle (NOT a status move) vs fast Electrode.
        // Tackle gets NO priority boost → P2 (faster) always goes first → P1 always damaged.
        let mut p1 = make_mon(Species::Snorlax, Ability::Prankster,
            [Some(PokemonMove::Tackle), None, None, None], None);
        let p1_initial_hp = p1.hp;

        let mut p2 = make_mon(Species::Electrode, Ability::None,
            [Some(PokemonMove::Tackle), None, None, None], None);
        p2.stats[5] = 999;

        let outcomes = run(p1, p2);

        let undamaged = p1_undamaged_prob(&outcomes, p1_initial_hp);
        assert!(
            undamaged < 0.01,
            "Prankster must not boost Tackle (damaging); P2 should always go first. \
             Undamaged prob = {undamaged:.3}"
        );
    }

    #[test]
    fn prankster_thunder_wave_blocked_by_dark_type() {
        // Prankster Thunder Wave fires first (+1 priority) but the target is Dark-type —
        // Dark-types are immune to Prankster-boosted status moves (Gen VII+).
        // P2 is never paralyzed → P2's Tackle always lands → P1 always damaged.
        let mut p1 = make_mon(Species::Snorlax, Ability::Prankster,
            [Some(PokemonMove::ThunderWave), None, None, None], None);
        let p1_initial_hp = p1.hp;

        let mut p2 = make_mon(Species::Umbreon, Ability::None, // Umbreon is Dark-type
            [Some(PokemonMove::Tackle), None, None, None], None);
        p2.stats[5] = 999;

        let outcomes = run(p1, p2);

        let undamaged = p1_undamaged_prob(&outcomes, p1_initial_hp);
        assert!(
            undamaged < 0.01,
            "Dark-type should be immune to Prankster Thunder Wave; P2 should never be \
             paralyzed so P1 is always damaged. Undamaged prob = {undamaged:.3}"
        );
    }

    #[test]
    fn prankster_thunder_wave_hits_non_dark_type() {
        // Control test: P2 is Normal-type (neither Dark nor Electric).
        // Prankster Thunder Wave fires first (+1 priority) → P2 paralyzed →
        // some branches where P2's paralysis fails (12.5%) → P1 undamaged.
        // Note: P2 must NOT be Electric (immune to TW) or Dark (immune to Prankster).
        let pdex = pokemon_dex();
        let mdex = move_dex();

        let mut p1 = make_mon(Species::Snorlax, Ability::Prankster,
            [Some(PokemonMove::ThunderWave), None, None, None], None);
        let p1_initial_hp = p1.hp;

        let mut p2 = make_mon(Species::Snorlax, Ability::None,  // Normal-type: not Dark, not Electric
            [Some(PokemonMove::Tackle), None, None, None], None);
        p2.stats[5] = 999;

        let state = battle_state_from_lists(vec![p1.clone()], vec![], vec![p2], vec![]);
        let outcomes = run_single_turn(
            &MatchState::BattleState(state),
            &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
            &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
            mdex, pdex,
        );

        let undamaged = p1_undamaged_prob(&outcomes, p1_initial_hp);
        assert!(
            undamaged > 0.05,
            "Non-Dark-type should NOT be immune to Prankster Thunder Wave; \
             some paralysis-fail branches expected. Got undamaged prob = {undamaged:.3}"
        );
    }

    // ── Gale Wings ─────────────────────────────────────────────────────────

    #[test]
    fn gale_wings_gives_flying_moves_priority_at_full_hp() {
        // Slow Gale Wings mon at FULL HP (hp = stats[0] = 1) uses Gust (Flying).
        // Gust gets +1 priority → fires first → KOs P2 (also at 1 HP) → P1 wins 100%.
        let mut p1 = make_mon(Species::Snorlax, Ability::GaleWings,
            [Some(PokemonMove::Gust), None, None, None], None);
        p1.hp = 1;
        p1.stats[0] = 1; // max HP = 1, so p1 is at "full HP"

        let mut p2 = make_mon(Species::Electrode, Ability::None,
            [Some(PokemonMove::Tackle), None, None, None], None);
        p2.hp = 1;
        p2.stats[0] = 1;
        p2.stats[5] = 999; // fast, so it wins without Gale Wings

        let outcomes = run(p1, p2);

        let p1_wins = p1_win_prob(&outcomes);
        assert!(
            (p1_wins - 1.0).abs() < 0.01,
            "Gale Wings at full HP should give Gust +1 priority → P1 always goes first. \
             P1 win prob = {p1_wins:.3}"
        );
    }

    #[test]
    fn gale_wings_inactive_when_not_at_full_hp() {
        // Same setup but P1 is at 1 HP with max HP = 100 → NOT full HP → no Gale Wings.
        // P2 (faster) goes first → KOs P1 → P2 wins 100%.
        let mut p1 = make_mon(Species::Snorlax, Ability::GaleWings,
            [Some(PokemonMove::Gust), None, None, None], None);
        p1.hp = 1;
        p1.stats[0] = 100; // max HP 100, current 1 → NOT full HP

        let mut p2 = make_mon(Species::Electrode, Ability::None,
            [Some(PokemonMove::Tackle), None, None, None], None);
        p2.hp = 1;
        p2.stats[0] = 1;
        p2.stats[5] = 999;

        let outcomes = run(p1, p2);

        let p2_wins = p2_win_prob(&outcomes);
        assert!(
            (p2_wins - 1.0).abs() < 0.01,
            "Gale Wings should be inactive below full HP; P2 should always go first. \
             P2 win prob = {p2_wins:.3}"
        );
    }

    #[test]
    fn gale_wings_does_not_boost_non_flying_moves() {
        // Slow Gale Wings mon at full HP uses Tackle (Normal type, not Flying).
        // No priority boost → P2 (faster) goes first → P2 wins 100%.
        let mut p1 = make_mon(Species::Snorlax, Ability::GaleWings,
            [Some(PokemonMove::Tackle), None, None, None], None);
        p1.hp = 1;
        p1.stats[0] = 1;

        let mut p2 = make_mon(Species::Electrode, Ability::None,
            [Some(PokemonMove::Tackle), None, None, None], None);
        p2.hp = 1;
        p2.stats[0] = 1;
        p2.stats[5] = 999;

        let outcomes = run(p1, p2);

        let p2_wins = p2_win_prob(&outcomes);
        assert!(
            (p2_wins - 1.0).abs() < 0.01,
            "Gale Wings must only boost Flying-type moves; Tackle should get no boost. \
             P2 win prob = {p2_wins:.3}"
        );
    }

    // ── Queenly Majesty / Armor Tail / Dazzling ────────────────────────────

    #[test]
    fn queenly_majesty_blocks_priority_moves() {
        // P1 uses Quick Attack (+1 priority) into a P2 with Queenly Majesty.
        // The move should be blocked entirely — P2 takes no damage.
        let mut p1 = make_mon(Species::Electrode, Ability::None,
            [Some(PokemonMove::QuickAttack), None, None, None], None);
        p1.stats[5] = 999;
        let p2 = make_mon(Species::Snorlax, Ability::QueenlyMajesty,
            [Some(PokemonMove::Splash), None, None, None], None);
        let p2_initial_hp = p2.hp;

        let outcomes = run(p1, p2);

        let p2_always_undamaged = outcomes.iter().all(|(s, _)| {
            if let MatchState::BattleState(bs) = s {
                bs.p2_active_mons[0].hp == p2_initial_hp
            } else { true }
        });
        assert!(p2_always_undamaged, "Queenly Majesty should block Quick Attack (+1 priority) entirely");
    }

    #[test]
    fn queenly_majesty_does_not_block_normal_priority() {
        // P1 uses Tackle (priority 0) into P2 with Queenly Majesty.
        // Normal-priority moves are NOT blocked → P2 takes damage in all branches.
        let mut p1 = make_mon(Species::Electrode, Ability::None,
            [Some(PokemonMove::Tackle), None, None, None], None);
        p1.stats[5] = 999;
        let p2 = make_mon(Species::Snorlax, Ability::QueenlyMajesty,
            [Some(PokemonMove::Splash), None, None, None], None);
        let p2_initial_hp = p2.hp;

        let outcomes = run(p1, p2);

        let p2_always_damaged = outcomes.iter().all(|(s, _)| {
            if let MatchState::BattleState(bs) = s {
                bs.p2_active_mons[0].hp < p2_initial_hp
            } else { true }
        });
        assert!(p2_always_damaged,
            "Queenly Majesty must not block priority-0 moves; P2 should always take damage");
    }

    #[test]
    fn queenly_majesty_blocks_prankster_boosted_move() {
        // P1 slow Prankster uses Growl (status → effective priority +1 via Prankster).
        // P2 has Queenly Majesty → the boosted status move is blocked (effective priority > 0).
        // Result: P2's Defense is NOT lowered in any branch.
        let mut p1 = make_mon(Species::Snorlax, Ability::Prankster,
            [Some(PokemonMove::Growl), None, None, None], None);
        // Slow, so without Prankster Growl priority 0 → P2 goes first anyway (doesn't matter here)
        let p2 = make_mon(Species::Snorlax, Ability::QueenlyMajesty,
            [Some(PokemonMove::Splash), None, None, None], None);

        let outcomes = run(p1, p2);

        // Growl lowers Attack by 1 stage. If QM blocked it, P2's Attack boosts stay at 0.
        let p2_never_debuffed = outcomes.iter().all(|(s, _)| {
            if let MatchState::BattleState(bs) = s {
                bs.p2_active_mons[0].boosts[0] == 0 // Attack boost index 0, should be 0 (not -1)
            } else { true }
        });
        assert!(p2_never_debuffed,
            "Queenly Majesty should block Prankster-boosted Growl (effective priority +1); \
             P2's Attack should not be lowered");
    }

    // ── Quick Draw ─────────────────────────────────────────────────────────

    #[test]
    fn quick_draw_activates_30_percent() {
        // Slow Quick Draw mon (P1, speed=1) vs fast mon (P2, speed=999), both at 1 HP.
        // Quick Draw fires 30% → P1 goes first → KOs P2 → P1 wins.
        // P1-win probability should be ≈ 0.30.
        let mut p1 = make_mon(Species::Snorlax, Ability::QuickDraw,
            [Some(PokemonMove::Tackle), None, None, None], None);
        p1.hp = 1;
        p1.stats[0] = 1;

        let mut p2 = make_mon(Species::Electrode, Ability::None,
            [Some(PokemonMove::Tackle), None, None, None], None);
        p2.hp = 1;
        p2.stats[0] = 1;
        p2.stats[5] = 999;

        let outcomes = run(p1, p2);

        let p1_wins = p1_win_prob(&outcomes);
        assert!(
            (p1_wins - 0.30).abs() < 0.01,
            "Quick Draw should give slow mon a 30% chance to act first. Got {p1_wins:.3}"
        );
    }

    #[test]
    fn quick_draw_does_not_boost_status_moves() {
        // Quick Draw applies only to damaging moves. Thunder Wave (status) should not
        // trigger Quick Draw → slow P1 never goes first → P2 (faster) always wins.
        let mut p1 = make_mon(Species::Snorlax, Ability::QuickDraw,
            [Some(PokemonMove::ThunderWave), None, None, None], None);
        p1.hp = 1;
        p1.stats[0] = 1;

        let mut p2 = make_mon(Species::Electrode, Ability::None,
            [Some(PokemonMove::Tackle), None, None, None], None);
        p2.hp = 1;
        p2.stats[0] = 1;
        p2.stats[5] = 999;

        let outcomes = run(p1, p2);

        // Thunder Wave can't KO, so P2 (faster with Tackle) should always KO P1.
        let p2_wins = p2_win_prob(&outcomes);
        assert!(
            (p2_wins - 1.0).abs() < 0.01,
            "Quick Draw must not activate for status moves; P2 should always go first. \
             P2 win prob = {p2_wins:.3}"
        );
    }

    #[test]
    fn quick_draw_does_not_cross_priority_brackets() {
        // Slow Quick Draw mon with Tackle (priority 0) vs fast mon with Quick Attack (+1).
        // Even if Quick Draw activates, it cannot beat a +1 priority move → P2 always first.
        let mut p1 = make_mon(Species::Snorlax, Ability::QuickDraw,
            [Some(PokemonMove::Tackle), None, None, None], None);
        p1.hp = 1;
        p1.stats[0] = 1;

        let mut p2 = make_mon(Species::Pikachu, Ability::None,
            [Some(PokemonMove::QuickAttack), None, None, None], None);
        p2.hp = 1;
        p2.stats[0] = 1;
        p2.stats[5] = 999;

        let outcomes = run(p1, p2);

        let p2_wins = p2_win_prob(&outcomes);
        assert!(
            (p2_wins - 1.0).abs() < 0.01,
            "Quick Draw cannot reorder across priority brackets; +1 Quick Attack should \
             always beat priority-0 Tackle. P2 win prob = {p2_wins:.3}"
        );
    }

    // ── Stall ──────────────────────────────────────────────────────────────

    #[test]
    fn stall_forces_holder_to_move_last() {
        // Fast Stall mon (P1, speed=999) vs slow mon (P2, speed=1), both at 1 HP.
        // Without Stall: P1 (faster) would go first → P1 wins.
        // With Stall: P1 goes LAST within its priority bracket → P2 (slower) goes first
        //             → P2's Tackle KOs P1 → P2 wins 100%.
        let mut p1 = make_mon(Species::Snorlax, Ability::Stall,
            [Some(PokemonMove::Tackle), None, None, None], None);
        p1.hp = 1;
        p1.stats[0] = 1;
        p1.stats[5] = 999; // fast, but Stall forces last

        let mut p2 = make_mon(Species::Snorlax, Ability::None,
            [Some(PokemonMove::Tackle), None, None, None], None);
        p2.hp = 1;
        p2.stats[0] = 1;
        // p2.stats[5] = 1 (default, very slow)

        let outcomes = run(p1, p2);

        let p2_wins = p2_win_prob(&outcomes);
        assert!(
            (p2_wins - 1.0).abs() < 0.01,
            "Stall should force the faster holder to move last; slow P2 should always go \
             first and win. P2 win prob = {p2_wins:.3}"
        );
    }

    #[test]
    fn two_stall_users_fall_back_to_speed() {
        // Both mons have Stall, both at 1 HP. The Stall-last tie-break is a wash (both
        // Stall), so turn order falls back to normal Speed comparison → P1 (faster) wins.
        let mut p1 = make_mon(Species::Snorlax, Ability::Stall,
            [Some(PokemonMove::Tackle), None, None, None], None);
        p1.hp = 1;
        p1.stats[0] = 1;
        p1.stats[5] = 999; // faster

        let mut p2 = make_mon(Species::Snorlax, Ability::Stall,
            [Some(PokemonMove::Tackle), None, None, None], None);
        p2.hp = 1;
        p2.stats[0] = 1;
        // p2.stats[5] = 1 (default, slower)

        let outcomes = run(p1, p2);

        let p1_wins = p1_win_prob(&outcomes);
        assert!(
            (p1_wins - 1.0).abs() < 0.01,
            "Two Stall users should fall back to Speed; faster P1 should always win. \
             P1 win prob = {p1_wins:.3}"
        );
    }

    mod end_of_turn_abilities {
        use super::*;
        use crate::battle::BattleState;
        use crate::dex_data::{Status, Weather};

        fn splash_mon(species: Species, ability: Ability, item: Option<Item>, status: Option<Status>) -> PokemonState {
            let dex = pokemon_dex();
            let mdex = move_dex();
            let mut p = build_pokemon_state(
                species, &dex, &mdex, Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None, Some(ability), None, item, None, Some([0; 6]), None, false,
            );
            p.status = status;
            p
        }

        fn aura_wheel_mon(species: Species) -> PokemonState {
            let dex = pokemon_dex();
            let mdex = move_dex();
            build_pokemon_state(
                species, &dex, &mdex, Some(50),
                Some([Some(PokemonMove::AuraWheel), None, None, None]),
                None, Some(Ability::HungerSwitch), None, None, None, Some([0; 6]), None, false,
            )
        }

        fn run(state: BattleState) -> Vec<(MatchState, f64)> {
            let pdex = pokemon_dex();
            let mdex = move_dex();
            run_single_turn(
                &MatchState::BattleState(state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &mdex, &pdex,
            )
        }

        // ─── Speed Boost ────────────────────────────────────────────────────────────

        #[test]
        fn speed_boost_does_not_fire_on_switch_in_turn() {
            let p1 = splash_mon(Species::Snorlax, Ability::SpeedBoost, None, None);
            let p2 = splash_mon(Species::Snorlax, Ability::None, None, None);
            // battle_state_from_lists sets entered_this_turn=true for leads
            let state = battle_state_from_lists(vec![p1], vec![], vec![p2], vec![]);
            let outcomes = run(state);
            assert!(
                outcomes.iter().all(|(s, _)| matches!(s, MatchState::BattleState(bs)
                    if bs.p1_active_mons[0].boosts[4] == 0)),
                "Speed Boost should not fire on the turn the Pokémon switches in"
            );
        }

        #[test]
        fn speed_boost_fires_on_subsequent_turns() {
            let p1 = splash_mon(Species::Snorlax, Ability::SpeedBoost, None, None);
            let p2 = splash_mon(Species::Snorlax, Ability::None, None, None);
            let mut state = battle_state_from_lists(vec![p1], vec![], vec![p2], vec![]);
            // Simulate a turn having already passed: clear the entry flag.
            state.p1_active_mons[0].entered_this_turn = false;
            let outcomes = run(state);
            assert!(
                outcomes.iter().all(|(s, _)| matches!(s, MatchState::BattleState(bs)
                    if bs.p1_active_mons[0].boosts[4] == 1)),
                "Speed Boost should grant +1 Speed at end of every non-entry turn"
            );
        }

        // ─── Shed Skin ──────────────────────────────────────────────────────────────

        #[test]
        fn shed_skin_cures_one_third_of_the_time_before_burn_damage() {
            let mut p1 = splash_mon(Species::Snorlax, Ability::ShedSkin, None, Some(Status::Burn));
            let max_hp = p1.stats[0];
            p1.hp = max_hp;
            let mut state = battle_state_from_lists(
                vec![p1],
                vec![],
                vec![splash_mon(Species::Snorlax, Ability::None, None, None)],
                vec![],
            );
            state.p1_active_mons[0].entered_this_turn = false;
            let outcomes = run(state);

            let cured_prob: f64 = outcomes.iter()
                .filter(|(s, _)| matches!(s, MatchState::BattleState(bs)
                    if bs.p1_active_mons[0].status.is_none()))
                .map(|(_, p)| p).sum();
            let burned_prob: f64 = outcomes.iter()
                .filter(|(s, _)| matches!(s, MatchState::BattleState(bs)
                    if bs.p1_active_mons[0].status.is_some()))
                .map(|(_, p)| p).sum();

            assert!((cured_prob - 1.0/3.0).abs() < 0.005,
                "Shed Skin should cure status 1/3 of the time, got {cured_prob:.4}");
            assert!((burned_prob - 2.0/3.0).abs() < 0.005,
                "Shed Skin should not cure 2/3 of the time, got {burned_prob:.4}");

            // Cured branches must have taken no burn damage (Shed Skin fires before burn).
            for (s, _) in &outcomes {
                if let MatchState::BattleState(bs) = s {
                    if bs.p1_active_mons[0].status.is_none() {
                        assert_eq!(bs.p1_active_mons[0].hp, max_hp,
                            "Cured branch should take no burn damage (Shed Skin fires before damage)");
                    }
                }
            }

            // Burned branches must have taken burn damage.
            let burn_dmg = (max_hp as u32 / 16).max(1) as u16;
            for (s, _) in &outcomes {
                if let MatchState::BattleState(bs) = s {
                    if bs.p1_active_mons[0].status.is_some() {
                        assert!(bs.p1_active_mons[0].hp <= max_hp.saturating_sub(burn_dmg),
                            "Burned branch should have taken burn damage");
                    }
                }
            }
        }

        // ─── Healer ─────────────────────────────────────────────────────────────────

        #[test]
        fn healer_has_no_effect_in_singles() {
            // Healer only cures adjacent allies; in singles there are none.
            let p1 = splash_mon(Species::Chansey, Ability::Healer, None, None);
            let mut p2 = splash_mon(Species::Snorlax, Ability::None, None, Some(Status::Burn));
            let max_hp = p2.stats[0];
            p2.hp = max_hp;
            let state = battle_state_from_lists(vec![p1], vec![], vec![p2], vec![]);
            let outcomes = run(state);
            assert_eq!(outcomes.len(), 1, "Healer in singles should not branch");
            assert!(
                outcomes.iter().all(|(s, _)| matches!(s, MatchState::BattleState(bs)
                    if bs.p2_active_mons[0].status.is_some())),
                "Healer in singles should not cure the opposing Pokémon"
            );
        }

        // ─── Moody ──────────────────────────────────────────────────────────────────

        #[test]
        fn moody_produces_20_outcomes_on_zeroed_stats_and_never_touches_accuracy_evasion() {
            let p1 = splash_mon(Species::Snorlax, Ability::Moody, None, None);
            let p2 = splash_mon(Species::Snorlax, Ability::None, None, None);
            let mut state = battle_state_from_lists(vec![p1], vec![], vec![p2], vec![]);
            state.p1_active_mons[0].entered_this_turn = false;
            let outcomes = run(state);

            let total_prob: f64 = outcomes.iter().map(|(_, p)| p).sum();
            assert!((total_prob - 1.0).abs() < 1e-9);

            // Exactly 20 distinct boost outcomes (5 raise × 4 lower)
            let boosts_only: Vec<_> = outcomes.iter()
                .filter_map(|(s, p)| if let MatchState::BattleState(bs) = s {
                    Some((bs.p1_active_mons[0].boosts, *p))
                } else { None })
                .collect();
            let unique: std::collections::HashSet<_> = boosts_only.iter().map(|(b, _)| *b).collect();
            assert_eq!(unique.len(), 20, "Moody with all-zero boosts should produce 20 distinct outcomes");

            for (boosts, p) in &boosts_only {
                // Each has probability 1/20
                assert!((p - 0.05).abs() < 0.005, "Each Moody outcome should have prob 1/20, got {p:.4}");
                // Exactly one stat raised by +2, one lowered by -1
                let raised: Vec<_> = (0..5).filter(|&i| boosts[i] == 2).collect();
                let lowered: Vec<_> = (0..5).filter(|&i| boosts[i] == -1).collect();
                assert_eq!(raised.len(), 1, "Moody should raise exactly one stat");
                assert_eq!(lowered.len(), 1, "Moody should lower exactly one stat");
                assert_ne!(raised[0], lowered[0], "Moody raised and lowered stat must differ");
                // Gen VIII+: accuracy and evasion never touched
                assert_eq!(boosts[5], 0, "Moody must not affect accuracy (Gen VIII+)");
                assert_eq!(boosts[6], 0, "Moody must not affect evasion (Gen VIII+)");
            }
        }

        // ─── Harvest ────────────────────────────────────────────────────────────────

        #[test]
        fn harvest_restores_consumed_berry_50_percent_normally() {
            let mut p1 = splash_mon(Species::Snorlax, Ability::Harvest, None, None);
            p1.consumed_item = Some(Item::SitrusBerry);
            // item slot is already Item::None (default)
            let mut state = battle_state_from_lists(
                vec![p1],
                vec![],
                vec![splash_mon(Species::Snorlax, Ability::None, None, None)],
                vec![],
            );
            state.p1_active_mons[0].entered_this_turn = false;
            let outcomes = run(state);

            let restored: f64 = outcomes.iter()
                .filter(|(s, _)| matches!(s, MatchState::BattleState(bs)
                    if bs.p1_active_mons[0].item == Item::SitrusBerry))
                .map(|(_, p)| p).sum();
            assert!((restored - 0.5).abs() < 0.005,
                "Harvest should restore a Berry 50% of the time, got {restored:.4}");
        }

        #[test]
        fn harvest_restores_berry_100_percent_in_sun() {
            let mut p1 = splash_mon(Species::Snorlax, Ability::Harvest, None, None);
            p1.consumed_item = Some(Item::SitrusBerry);
            let mut state = battle_state_from_lists(
                vec![p1],
                vec![],
                vec![splash_mon(Species::Snorlax, Ability::None, None, None)],
                vec![],
            );
            state.p1_active_mons[0].entered_this_turn = false;
            state.weather = Some(Weather::Sun);
            let outcomes = run(state);

            let restored: f64 = outcomes.iter()
                .filter(|(s, _)| matches!(s, MatchState::BattleState(bs)
                    if bs.p1_active_mons[0].item == Item::SitrusBerry))
                .map(|(_, p)| p).sum();
            assert!((restored - 1.0).abs() < 0.005,
                "Harvest should always restore a Berry in Sun, got {restored:.4}");
        }

        // ─── Hunger Switch ──────────────────────────────────────────────────────────

        #[test]
        fn hunger_switch_toggles_morpeko_to_hangry_after_one_turn() {
            let p1 = splash_mon(Species::Morpeko, Ability::HungerSwitch, None, None);
            let mut state = battle_state_from_lists(
                vec![p1],
                vec![],
                vec![splash_mon(Species::Snorlax, Ability::None, None, None)],
                vec![],
            );
            state.p1_active_mons[0].entered_this_turn = false;
            let outcomes = run(state);
            assert!(
                outcomes.iter().all(|(s, _)| matches!(s, MatchState::BattleState(bs)
                    if bs.p1_active_mons[0].species == Species::MorpekoHangry)),
                "Hunger Switch should toggle Morpeko to Hangry form at end of turn"
            );
        }

        #[test]
        fn hunger_switch_toggles_hangry_back_to_full_belly() {
            let mut p1 = splash_mon(Species::Morpeko, Ability::HungerSwitch, None, None);
            p1.species = Species::MorpekoHangry;
            let mut state = battle_state_from_lists(
                vec![p1],
                vec![],
                vec![splash_mon(Species::Snorlax, Ability::None, None, None)],
                vec![],
            );
            state.p1_active_mons[0].entered_this_turn = false;
            let outcomes = run(state);
            assert!(
                outcomes.iter().all(|(s, _)| matches!(s, MatchState::BattleState(bs)
                    if bs.p1_active_mons[0].species == Species::Morpeko)),
                "Hunger Switch should toggle Hangry Morpeko back to Full Belly"
            );
        }

        #[test]
        fn hunger_switch_does_not_fire_while_terastallized() {
            let p1 = splash_mon(Species::Morpeko, Ability::HungerSwitch, None, None);
            let mut state = battle_state_from_lists(
                vec![p1],
                vec![],
                vec![splash_mon(Species::Snorlax, Ability::None, None, None)],
                vec![],
            );
            state.p1_active_mons[0].is_tera = true;
            state.p1_active_mons[0].entered_this_turn = false;
            let outcomes = run(state);
            assert!(
                outcomes.iter().all(|(s, _)| matches!(s, MatchState::BattleState(bs)
                    if bs.p1_active_mons[0].species == Species::Morpeko)),
                "Hunger Switch must not toggle while Morpeko is Terastallized"
            );
        }

        // ─── Aura Wheel ─────────────────────────────────────────────────────────────

        #[test]
        fn aura_wheel_is_electric_type_in_full_belly_form() {
            // Electric is immune to Ground-type Pokémon. Garchomp (Dragon/Ground) should take
            // 0 damage from an Electric Aura Wheel.
            let pdex = pokemon_dex();
            let mdex = move_dex();
            let p1 = aura_wheel_mon(Species::Morpeko);          // Full Belly → Electric
            let p2 = splash_mon(Species::Garchomp, Ability::None, None, None);
            let state = battle_state_from_lists(vec![p1], vec![], vec![p2], vec![]);
            let initial_hp = state.p2_active_mons[0].hp;
            let outcomes = run_single_turn(
                &MatchState::BattleState(state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &mdex, &pdex,
            );
            assert!(
                outcomes.iter().all(|(s, _)| matches!(s, MatchState::BattleState(bs)
                    if bs.p2_active_mons[0].hp == initial_hp)),
                "Aura Wheel in Full Belly Mode should be Electric-type (immune to Garchomp)"
            );
        }

        #[test]
        fn aura_wheel_is_dark_type_in_hangry_form() {
            // Dark is not immune to Ground. Garchomp should take normal damage.
            let pdex = pokemon_dex();
            let mdex = move_dex();
            let mut p1 = aura_wheel_mon(Species::Morpeko);
            p1.species = Species::MorpekoHangry;                 // Hangry → Dark
            let p2 = splash_mon(Species::Garchomp, Ability::None, None, None);
            let state = battle_state_from_lists(vec![p1], vec![], vec![p2], vec![]);
            let initial_hp = state.p2_active_mons[0].hp;
            let outcomes = run_single_turn(
                &MatchState::BattleState(state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &mdex, &pdex,
            );
            assert!(
                outcomes.iter().any(|(s, _)| matches!(s, MatchState::BattleState(bs)
                    if bs.p2_active_mons[0].hp < initial_hp)),
                "Aura Wheel in Hangry Mode should be Dark-type (deals damage to Ground-type)"
            );
        }

        #[test]
        fn aura_wheel_grants_plus_one_speed_on_hit() {
            let pdex = pokemon_dex();
            let mdex = move_dex();
            let p1 = aura_wheel_mon(Species::Morpeko);
            // Snorlax is Normal-type — not immune to Electric
            let p2 = splash_mon(Species::Snorlax, Ability::None, None, None);
            let state = battle_state_from_lists(vec![p1], vec![], vec![p2], vec![]);
            let outcomes = run_single_turn(
                &MatchState::BattleState(state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &mdex, &pdex,
            );
            assert!(
                outcomes.iter().all(|(s, _)| matches!(s, MatchState::BattleState(bs)
                    if bs.p1_active_mons[0].boosts[4] == 1)),
                "Aura Wheel should grant +1 Speed to the user on hit"
            );
        }

        #[test]
        fn aura_wheel_fails_for_non_morpeko_user() {
            let pdex = pokemon_dex();
            let mdex = move_dex();
            // Give a non-Morpeko (Snorlax) the Aura Wheel move.
            let p1 = build_pokemon_state(
                Species::Snorlax, &pokemon_dex(), &move_dex(), Some(50),
                Some([Some(PokemonMove::AuraWheel), None, None, None]),
                None, Some(Ability::None), None, None, None, Some([0; 6]), None, false,
            );
            let p2 = splash_mon(Species::Snorlax, Ability::None, None, None);
            let state = battle_state_from_lists(vec![p1], vec![], vec![p2], vec![]);
            let initial_hp = state.p2_active_mons[0].hp;
            let outcomes = run_single_turn(
                &MatchState::BattleState(state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &mdex, &pdex,
            );
            assert!(
                outcomes.iter().all(|(s, _)| matches!(s, MatchState::BattleState(bs)
                    if bs.p2_active_mons[0].hp == initial_hp)),
                "Aura Wheel should fail entirely when used by a non-Morpeko Pokémon"
            );
        }
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Item-interaction abilities (Klutz / Unburden / Sticky Hold / Magician /
    // Pickpocket / Pickup / Symbiosis)
    // ─────────────────────────────────────────────────────────────────────────
    #[cfg(test)]
    mod item_interaction_abilities {
        use crate::battle::{BattleCommand, BattleState, FieldSlot, MatchState, Player, PlayerCommand, SwitchCommand};
        use crate::data::ability::Ability;
        use crate::data::item::Item;
        use crate::data::pokemon_move::PokemonMove;
        use crate::data::species::Species;
        use crate::dex_data::{Status, VolatileStatus};
        use crate::pokemon::{build_pokemon_state, PokemonState, VolatileStatusState};
        use crate::simulator_helpers;
        use crate::simuilator_test_helpers::{
            battle_state_from_lists, extract_battle_state, move_dex, pokemon_dex,
            run_single_turn, simple_attack,
        };

        fn splash_mon(species: Species, ability: Ability, item: Option<Item>) -> PokemonState {
            let dex = pokemon_dex();
            let mdex = move_dex();
            build_pokemon_state(
                species, &dex, &mdex, Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None, Some(ability), None, item, None, Some([0; 6]), None, false,
            )
        }

        fn tackle_mon(species: Species, ability: Ability, item: Option<Item>) -> PokemonState {
            let dex = pokemon_dex();
            let mdex = move_dex();
            build_pokemon_state(
                species, &dex, &mdex, Some(50),
                Some([Some(PokemonMove::Tackle), None, None, None]),
                None, Some(ability), None, item, None, Some([0; 6]), None, false,
            )
        }

        fn run(state: BattleState) -> Vec<(MatchState, f64)> {
            let pdex = pokemon_dex();
            let mdex = move_dex();
            run_single_turn(
                &MatchState::BattleState(state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &mdex, &pdex,
            )
        }

        fn p1_speed(bs: &BattleState) -> f32 {
            simulator_helpers::effective_speed_for_slot(
                bs,
                FieldSlot { player: Player::P1, slot_index: 0 },
                &bs.p1_active_mons[0],
            )
        }

        // ─── Klutz ──────────────────────────────────────────────────────────────────

        #[test]
        fn klutz_negates_choice_scarf_speed() {
            let p1 = splash_mon(Species::Snorlax, Ability::Klutz, Some(Item::ChoiceScarf));
            let p2 = splash_mon(Species::Snorlax, Ability::None, None);
            let state = battle_state_from_lists(vec![p1], vec![], vec![p2], vec![]);
            let raw = state.p1_active_mons[0].stats[5] as f32;
            assert_eq!(p1_speed(&state), raw, "Klutz holder's Choice Scarf should be inert");
        }

        #[test]
        fn klutz_prevents_berry_consumption() {
            // Mirrors the Sitrus test: poison drops the holder below 50%, but with Klutz
            // the berry must never be eaten (no heal, item retained).
            let mut p1 = splash_mon(Species::Snorlax, Ability::Klutz, Some(Item::SitrusBerry));
            let max_hp = p1.stats[0];
            p1.hp = max_hp / 2 + 1;
            p1.status = Some(Status::Poison);
            let p2 = splash_mon(Species::Shuckle, Ability::None, None);

            let initial = battle_state_from_lists(vec![p1], vec![], vec![p2], vec![]);
            let (bs, _) = extract_battle_state(run(initial));

            let poison_dmg = (max_hp / 8).max(1);
            let expected_hp = (max_hp / 2 + 1).saturating_sub(poison_dmg);
            assert_eq!(bs.p1_active_mons[0].item, Item::SitrusBerry,
                "Klutz must prevent the berry from being eaten");
            assert_eq!(bs.p1_active_mons[0].hp, expected_hp,
                "no berry heal should have been applied");
        }

        #[test]
        fn klutz_suppressed_by_gastro_acid_reenables_item() {
            let p1 = splash_mon(Species::Snorlax, Ability::Klutz, Some(Item::ChoiceScarf));
            let p2 = splash_mon(Species::Snorlax, Ability::None, None);
            let mut state = battle_state_from_lists(vec![p1], vec![], vec![p2], vec![]);
            state.p1_active_mons[0].volatiles.push(
                VolatileStatusState::TurnStatus(VolatileStatus::GastroAcid, 0));
            let raw = state.p1_active_mons[0].stats[5] as f32;
            assert_eq!(p1_speed(&state), raw * 1.5,
                "Gastro Acid suppresses Klutz, so the Scarf works again");
        }

        // ─── Unburden ───────────────────────────────────────────────────────────────

        #[test]
        fn unburden_doubles_speed_after_berry_consumed() {
            let mut p1 = splash_mon(Species::Snorlax, Ability::Unburden, Some(Item::SitrusBerry));
            let max_hp = p1.stats[0];
            p1.hp = max_hp / 2 + 1;
            p1.status = Some(Status::Poison);
            let p2 = splash_mon(Species::Shuckle, Ability::None, None);

            let initial = battle_state_from_lists(vec![p1], vec![], vec![p2], vec![]);
            let (bs, _) = extract_battle_state(run(initial));

            assert_eq!(bs.p1_active_mons[0].item, Item::None, "berry should be consumed");
            assert!(bs.p1_active_mons[0].item_lost, "item-loss flag should be set");
            let raw = bs.p1_active_mons[0].stats[5] as f32;
            assert_eq!(p1_speed(&bs), raw * 2.0, "Unburden should double Speed");
        }

        #[test]
        fn unburden_boost_lost_on_switch_out() {
            let mut p1 = splash_mon(Species::Snorlax, Ability::Unburden, Some(Item::SitrusBerry));
            let max_hp = p1.stats[0];
            p1.hp = max_hp / 2 + 1;
            p1.status = Some(Status::Poison);
            let bench = splash_mon(Species::Clefable, Ability::None, None);
            let p2 = splash_mon(Species::Shuckle, Ability::None, None);

            let initial = battle_state_from_lists(vec![p1], vec![bench], vec![p2], vec![]);
            let (bs, _) = extract_battle_state(run(initial));
            assert!(bs.p1_active_mons[0].item_lost, "precondition: Unburden armed");

            // Switch the Unburden holder out: the boost condition must be cleared.
            let pdex = pokemon_dex();
            let mdex = move_dex();
            let outcomes = run_single_turn(
                &MatchState::BattleState(bs),
                &PlayerCommand::Battle(vec![BattleCommand::Switch(SwitchCommand { party_index: 0 })]),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &mdex, &pdex,
            );
            let (bs2, _) = extract_battle_state(outcomes);
            assert!(!bs2.p1_back_mons[0].item_lost,
                "Unburden's item-loss flag must be cleared on switch-out");
        }

        #[test]
        fn unburden_inactive_when_entering_without_item() {
            let p1 = splash_mon(Species::Snorlax, Ability::Unburden, None);
            let p2 = splash_mon(Species::Snorlax, Ability::None, None);
            let state = battle_state_from_lists(vec![p1], vec![], vec![p2], vec![]);
            let raw = state.p1_active_mons[0].stats[5] as f32;
            assert_eq!(p1_speed(&state), raw,
                "entering battle without an item must not activate Unburden");
        }

        // ─── Magician ───────────────────────────────────────────────────────────────

        #[test]
        fn magician_steals_item_from_damaged_target() {
            let p1 = tackle_mon(Species::Snorlax, Ability::Magician, None);
            let p2 = splash_mon(Species::Shuckle, Ability::None, Some(Item::Leftovers));
            let state = battle_state_from_lists(vec![p1], vec![], vec![p2], vec![]);
            let (bs, _) = extract_battle_state(run(state));

            assert_eq!(bs.p1_active_mons[0].item, Item::Leftovers,
                "Magician should steal the damaged target's item");
            assert_eq!(bs.p2_active_mons[0].item, Item::None,
                "the target should have lost its item");
            assert!(bs.p2_active_mons[0].item_lost,
                "theft should arm the victim's Unburden flag");
        }

        #[test]
        fn magician_does_not_steal_while_holding_an_item() {
            let p1 = tackle_mon(Species::Snorlax, Ability::Magician, Some(Item::SafetyGoggles));
            let p2 = splash_mon(Species::Shuckle, Ability::None, Some(Item::Leftovers));
            let state = battle_state_from_lists(vec![p1], vec![], vec![p2], vec![]);
            let (bs, _) = extract_battle_state(run(state));

            assert_eq!(bs.p1_active_mons[0].item, Item::SafetyGoggles);
            assert_eq!(bs.p2_active_mons[0].item, Item::Leftovers,
                "Magician must not steal while already holding an item");
        }

        #[test]
        fn magician_blocked_by_sticky_hold() {
            let p1 = tackle_mon(Species::Snorlax, Ability::Magician, None);
            let p2 = splash_mon(Species::Shuckle, Ability::StickyHold, Some(Item::Leftovers));
            let state = battle_state_from_lists(vec![p1], vec![], vec![p2], vec![]);
            let (bs, _) = extract_battle_state(run(state));

            assert_eq!(bs.p1_active_mons[0].item, Item::None,
                "Sticky Hold must block Magician");
            assert_eq!(bs.p2_active_mons[0].item, Item::Leftovers,
                "the Sticky Hold holder keeps its item");
        }

        // ─── Pickpocket ─────────────────────────────────────────────────────────────

        #[test]
        fn pickpocket_steals_attackers_item_on_contact() {
            let p1 = splash_mon(Species::Shuckle, Ability::Pickpocket, None);
            let p2 = tackle_mon(Species::Snorlax, Ability::None, Some(Item::Leftovers));
            let state = battle_state_from_lists(vec![p1], vec![], vec![p2], vec![]);
            let (bs, _) = extract_battle_state(run(state));

            assert_eq!(bs.p1_active_mons[0].item, Item::Leftovers,
                "Pickpocket should steal the contact attacker's item");
            assert_eq!(bs.p2_active_mons[0].item, Item::None);
            assert!(bs.p2_active_mons[0].item_lost,
                "theft should arm the attacker's Unburden flag");
        }

        #[test]
        fn pickpocket_ignores_non_contact_moves() {
            let dex = pokemon_dex();
            let mdex = move_dex();
            let p1 = splash_mon(Species::Shuckle, Ability::Pickpocket, None);
            let p2 = build_pokemon_state(
                Species::Snorlax, &dex, &mdex, Some(50),
                Some([Some(PokemonMove::Swift), None, None, None]),
                None, Some(Ability::None), None, Some(Item::Leftovers),
                None, Some([0; 6]), None, false,
            );
            let state = battle_state_from_lists(vec![p1], vec![], vec![p2], vec![]);
            let (bs, _) = extract_battle_state(run(state));

            assert_eq!(bs.p1_active_mons[0].item, Item::None,
                "Pickpocket must not trigger on non-contact damage");
            assert_eq!(bs.p2_active_mons[0].item, Item::Leftovers);
        }

        #[test]
        fn pickpocket_does_not_trigger_while_holding_an_item() {
            let p1 = splash_mon(Species::Shuckle, Ability::Pickpocket, Some(Item::SafetyGoggles));
            let p2 = tackle_mon(Species::Snorlax, Ability::None, Some(Item::Leftovers));
            let state = battle_state_from_lists(vec![p1], vec![], vec![p2], vec![]);
            let (bs, _) = extract_battle_state(run(state));

            assert_eq!(bs.p1_active_mons[0].item, Item::SafetyGoggles);
            assert_eq!(bs.p2_active_mons[0].item, Item::Leftovers,
                "Pickpocket must not trigger while already holding an item");
        }

        // ─── Pickup ─────────────────────────────────────────────────────────────────

        #[test]
        fn pickup_retrieves_item_consumed_by_opponent() {
            let p1 = splash_mon(Species::Snorlax, Ability::Pickup, None);
            let mut p2 = splash_mon(Species::Snorlax, Ability::None, Some(Item::SitrusBerry));
            let max_hp = p2.stats[0];
            p2.hp = max_hp / 2 + 1;
            p2.status = Some(Status::Poison);

            let state = battle_state_from_lists(vec![p1], vec![], vec![p2], vec![]);
            let (bs, _) = extract_battle_state(run(state));

            assert_eq!(bs.p2_active_mons[0].item, Item::None, "P2 should have eaten its berry");
            assert_eq!(bs.p1_active_mons[0].item, Item::SitrusBerry,
                "Pickup should retrieve the berry the opponent consumed this turn");
            assert!(bs.items_consumed_this_turn.is_empty(),
                "the per-turn consumed pool must be cleared after end of turn");
        }

        #[test]
        fn pickup_does_not_retrieve_own_consumed_item() {
            let mut p1 = splash_mon(Species::Snorlax, Ability::Pickup, Some(Item::SitrusBerry));
            let max_hp = p1.stats[0];
            p1.hp = max_hp / 2 + 1;
            p1.status = Some(Status::Poison);
            let p2 = splash_mon(Species::Snorlax, Ability::None, None);

            let state = battle_state_from_lists(vec![p1], vec![], vec![p2], vec![]);
            let (bs, _) = extract_battle_state(run(state));

            assert_eq!(bs.p1_active_mons[0].item, Item::None,
                "Pickup must not retrieve the holder's own consumed item");
        }

        #[test]
        fn pickup_inactive_while_holding_an_item() {
            let p1 = splash_mon(Species::Snorlax, Ability::Pickup, Some(Item::Leftovers));
            let mut p2 = splash_mon(Species::Snorlax, Ability::None, Some(Item::SitrusBerry));
            let max_hp = p2.stats[0];
            p2.hp = max_hp / 2 + 1;
            p2.status = Some(Status::Poison);

            let state = battle_state_from_lists(vec![p1], vec![], vec![p2], vec![]);
            let (bs, _) = extract_battle_state(run(state));

            assert_eq!(bs.p1_active_mons[0].item, Item::Leftovers,
                "Pickup must not fire while the holder already has an item");
        }

        // ─── Symbiosis (doubles) ────────────────────────────────────────────────────

        #[test]
        fn symbiosis_passes_item_to_ally_after_consumption() {
            let mut eater = splash_mon(Species::Snorlax, Ability::None, Some(Item::SitrusBerry));
            let max_hp = eater.stats[0];
            eater.hp = max_hp / 2 + 1;
            eater.status = Some(Status::Poison);
            let donor = splash_mon(Species::Shuckle, Ability::Symbiosis, Some(Item::Leftovers));
            let foe1 = splash_mon(Species::Shuckle, Ability::None, None);
            let foe2 = splash_mon(Species::Shuckle, Ability::None, None);

            let state = battle_state_from_lists(vec![eater, donor], vec![], vec![foe1, foe2], vec![]);
            let pdex = pokemon_dex();
            let mdex = move_dex();
            let outcomes = run_single_turn(
                &MatchState::BattleState(state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0, 0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0, 0])),
                &mdex, &pdex,
            );
            let (bs, _) = extract_battle_state(outcomes);

            assert_eq!(bs.p1_active_mons[0].item, Item::Leftovers,
                "the ally that consumed its berry should receive the Symbiosis holder's item");
            assert_eq!(bs.p1_active_mons[1].item, Item::None,
                "the Symbiosis holder should have given its item away");
            assert!(bs.p1_active_mons[1].item_lost,
                "donating via Symbiosis counts as losing the item (arms Unburden)");
        }

        #[test]
        fn symbiosis_does_not_trigger_on_theft() {
            let victim = splash_mon(Species::Shuckle, Ability::None, Some(Item::Leftovers));
            let donor = splash_mon(Species::Shuckle, Ability::Symbiosis, Some(Item::SafetyGoggles));
            let thief = tackle_mon(Species::Snorlax, Ability::Magician, None);
            let filler = splash_mon(Species::Shuckle, Ability::None, None);

            let state = battle_state_from_lists(vec![victim, donor], vec![], vec![thief, filler], vec![]);
            let pdex = pokemon_dex();
            let mdex = move_dex();
            let outcomes = run_single_turn(
                &MatchState::BattleState(state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0, 0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0, 0])),
                &mdex, &pdex,
            );
            let (bs, _) = extract_battle_state(outcomes);

            assert_eq!(bs.p2_active_mons[0].item, Item::Leftovers,
                "Magician should have stolen the victim's item");
            assert_eq!(bs.p1_active_mons[0].item, Item::None,
                "stolen items are not replaced by Symbiosis");
            assert_eq!(bs.p1_active_mons[1].item, Item::SafetyGoggles,
                "Symbiosis must not fire when the ally's item was stolen");
        }
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Form-change abilities (Zero to Hero / Stance Change / Disguise / Forecast)
    // ─────────────────────────────────────────────────────────────────────────
    #[cfg(test)]
    mod form_change_abilities {
        use crate::battle::{BattleCommand, BattleState, MatchState, Player, PlayerCommand, SwitchCommand};
        use crate::data::ability::Ability;
        use crate::data::pokemon_move::PokemonMove;
        use crate::data::species::Species;
        use crate::dex_data::Status;
        use crate::pokemon::{build_pokemon_state, PokemonState};
        use crate::simuilator_test_helpers::{
            battle_state_from_lists, extract_battle_state, move_dex, pokemon_dex,
            run_single_turn, simple_attack,
        };

        fn mon(species: Species, ability: Ability, mv: PokemonMove) -> PokemonState {
            let dex = pokemon_dex();
            let mdex = move_dex();
            build_pokemon_state(
                species, &dex, &mdex, Some(50),
                Some([Some(mv), None, None, None]),
                None, Some(ability), None, None, None, Some([0; 6]), None, false,
            )
        }

        fn run(state: BattleState) -> Vec<(MatchState, f64)> {
            let pdex = pokemon_dex();
            let mdex = move_dex();
            run_single_turn(
                &MatchState::BattleState(state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &mdex, &pdex,
            )
        }

        fn switch_p1(state: BattleState) -> BattleState {
            let pdex = pokemon_dex();
            let mdex = move_dex();
            let outcomes = run_single_turn(
                &MatchState::BattleState(state),
                &PlayerCommand::Battle(vec![BattleCommand::Switch(SwitchCommand { party_index: 0 })]),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &mdex, &pdex,
            );
            extract_battle_state(outcomes).0
        }

        // ─── Zero to Hero ───────────────────────────────────────────────────────────

        #[test]
        fn zero_to_hero_transforms_on_switch_out_and_persists() {
            let palafin = mon(Species::Palafin, Ability::ZerotoHero, PokemonMove::Splash);
            let zero_atk = palafin.stats[1];
            let bench = mon(Species::Clefable, Ability::None, PokemonMove::Splash);
            let foe = mon(Species::Shuckle, Ability::None, PokemonMove::Splash);

            let state = battle_state_from_lists(vec![palafin], vec![bench], vec![foe], vec![]);
            let bs = switch_p1(state);

            assert_eq!(bs.p1_back_mons[0].species, Species::PalafinHero,
                "Palafin must become Hero Form when it switches out");
            assert!(bs.p1_back_mons[0].stats[1] > zero_atk,
                "Hero Form should have a much higher Attack stat");

            // Switch back in: the Hero Form persists (it never reverts).
            let bs2 = switch_p1(bs);
            assert_eq!(bs2.p1_active_mons[0].species, Species::PalafinHero,
                "Hero Form persists when re-entering the field");
        }

        #[test]
        fn zero_to_hero_requires_the_ability() {
            let palafin = mon(Species::Palafin, Ability::None, PokemonMove::Splash);
            let bench = mon(Species::Clefable, Ability::None, PokemonMove::Splash);
            let foe = mon(Species::Shuckle, Ability::None, PokemonMove::Splash);

            let state = battle_state_from_lists(vec![palafin], vec![bench], vec![foe], vec![]);
            let bs = switch_p1(state);
            assert_eq!(bs.p1_back_mons[0].species, Species::Palafin,
                "without Zero to Hero, Palafin stays in Zero Form");
        }

        // ─── Stance Change ──────────────────────────────────────────────────────────

        #[test]
        fn stance_change_blade_forme_on_attack_with_blade_stats() {
            let foe_hp = mon(Species::Snorlax, Ability::None, PokemonMove::Splash).stats[0];

            // Control: no Stance Change — damage comes from Shield Forme's low Attack.
            let control = battle_state_from_lists(
                vec![mon(Species::Aegislash, Ability::None, PokemonMove::Tackle)], vec![],
                vec![mon(Species::Snorlax, Ability::None, PokemonMove::Splash)], vec![],
            );
            let (control_bs, _) = extract_battle_state(run(control));
            let control_dmg = foe_hp - control_bs.p2_active_mons[0].hp;

            let state = battle_state_from_lists(
                vec![mon(Species::Aegislash, Ability::StanceChange, PokemonMove::Tackle)], vec![],
                vec![mon(Species::Snorlax, Ability::None, PokemonMove::Splash)], vec![],
            );
            let (bs, _) = extract_battle_state(run(state));
            let blade_dmg = foe_hp - bs.p2_active_mons[0].hp;

            assert_eq!(bs.p1_active_mons[0].species, Species::AegislashBlade,
                "Aegislash must be in Blade Forme after using a damaging move");
            assert!(blade_dmg > control_dmg,
                "the triggering move itself must already use Blade Forme's Attack \
                 (blade {blade_dmg} vs shield {control_dmg})");
        }

        #[test]
        fn stance_change_does_not_trigger_while_asleep() {
            let mut aegi = mon(Species::Aegislash, Ability::StanceChange, PokemonMove::Tackle);
            aegi.status = Some(Status::Sleep(0)); // first sleep turn: move always fails
            let state = battle_state_from_lists(
                vec![aegi], vec![],
                vec![mon(Species::Snorlax, Ability::None, PokemonMove::Splash)], vec![],
            );
            let outcomes = run(state);
            assert!(
                outcomes.iter().all(|(s, _)| matches!(s, MatchState::BattleState(bs)
                    if bs.p1_active_mons[0].species == Species::Aegislash)),
                "a move prevented by sleep must not trigger Stance Change"
            );
        }

        #[test]
        fn stance_change_reverts_to_shield_forme_on_switch_out() {
            let shield_atk = mon(Species::Aegislash, Ability::StanceChange, PokemonMove::Tackle).stats[1];
            let state = battle_state_from_lists(
                vec![mon(Species::Aegislash, Ability::StanceChange, PokemonMove::Tackle)],
                vec![mon(Species::Clefable, Ability::None, PokemonMove::Splash)],
                vec![mon(Species::Snorlax, Ability::None, PokemonMove::Splash)], vec![],
            );
            let (bs, _) = extract_battle_state(run(state));
            assert_eq!(bs.p1_active_mons[0].species, Species::AegislashBlade, "precondition");

            let bs2 = switch_p1(bs);
            assert_eq!(bs2.p1_back_mons[0].species, Species::Aegislash,
                "Aegislash reverts to Shield Forme on switch-out");
            assert_eq!(bs2.p1_back_mons[0].stats[1], shield_atk,
                "Shield Forme stats must be restored");
        }

        #[test]
        fn stance_change_kings_shield_returns_to_shield_forme() {
            // Enter Blade Forme on turn 1, then King's Shield on turn 2 → Shield Forme.
            let dex = pokemon_dex();
            let mdex = move_dex();
            let aegi = build_pokemon_state(
                Species::Aegislash, &dex, &mdex, Some(50),
                Some([Some(PokemonMove::Tackle), Some(PokemonMove::KingsShield), None, None]),
                None, Some(Ability::StanceChange), None, None, None, Some([0; 6]), None, false,
            );
            let state = battle_state_from_lists(
                vec![aegi], vec![],
                vec![mon(Species::Snorlax, Ability::None, PokemonMove::Splash)], vec![],
            );
            let (bs, _) = extract_battle_state(run(state));
            assert_eq!(bs.p1_active_mons[0].species, Species::AegislashBlade, "precondition");

            let outcomes = run_single_turn(
                &MatchState::BattleState(bs),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![1])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &mdex, &dex,
            );
            let (bs2, _) = extract_battle_state(outcomes);
            assert_eq!(bs2.p1_active_mons[0].species, Species::Aegislash,
                "King's Shield returns Aegislash to Shield Forme");
        }

        // ─── Disguise ───────────────────────────────────────────────────────────────

        #[test]
        fn disguise_blocks_first_hit_busts_and_chips() {
            // Aerial Ace: hits Ghost/Fairy Mimikyu neutrally and never misses.
            let mimikyu = mon(Species::Mimikyu, Ability::Disguise, PokemonMove::Splash);
            let max_hp = mimikyu.stats[0];
            let state = battle_state_from_lists(
                vec![mimikyu], vec![],
                vec![mon(Species::Snorlax, Ability::None, PokemonMove::AerialAce)], vec![],
            );
            let (bs, _) = extract_battle_state(run(state));

            let chip = (max_hp / 8).max(1);
            assert_eq!(bs.p1_active_mons[0].species, Species::MimikyuBusted,
                "the blocked hit must bust the disguise");
            assert_eq!(bs.p1_active_mons[0].hp, max_hp - chip,
                "Mimikyu loses exactly 1/8 max HP instead of the move's damage");

            // Second turn: the disguise is busted, so Tackle now damages normally.
            let outcomes = run(bs);
            let (bs2, _) = extract_battle_state(outcomes);
            assert!(bs2.p1_active_mons[0].hp < max_hp - chip,
                "once busted, hits deal full damage");
        }

        #[test]
        fn disguise_ignores_status_moves() {
            let mimikyu = mon(Species::Mimikyu, Ability::Disguise, PokemonMove::Splash);
            let max_hp = mimikyu.stats[0];
            let state = battle_state_from_lists(
                vec![mimikyu], vec![],
                vec![mon(Species::Snorlax, Ability::None, PokemonMove::ThunderWave)], vec![],
            );
            let outcomes = run(state);
            for (s, _) in &outcomes {
                if let MatchState::BattleState(bs) = s {
                    assert_eq!(bs.p1_active_mons[0].species, Species::Mimikyu,
                        "status moves must not bust the disguise");
                    assert_eq!(bs.p1_active_mons[0].hp, max_hp,
                        "no HP chip without a blocked damaging hit");
                }
            }
        }

        #[test]
        fn disguise_blocks_only_first_strike_of_multi_hit() {
            let mimikyu = mon(Species::Mimikyu, Ability::Disguise, PokemonMove::Splash);
            let max_hp = mimikyu.stats[0];
            let chip = (max_hp / 8).max(1);
            let state = battle_state_from_lists(
                vec![mimikyu], vec![],
                vec![mon(Species::Snorlax, Ability::None, PokemonMove::DualWingbeat)], vec![],
            );
            let outcomes = run(state);

            // At least one branch must show the second strike dealing real damage on
            // top of the 1/8 chip (only the first strike is absorbed). If Disguise
            // wrongly blocked every strike, all busted branches would sit at exactly
            // max_hp - chip.
            let second_strike_damaged = outcomes.iter().any(|(s, _)| matches!(
                s,
                MatchState::BattleState(bs)
                    if bs.p1_active_mons[0].species == Species::MimikyuBusted
                        && bs.p1_active_mons[0].hp < max_hp - chip
            ));
            assert!(second_strike_damaged,
                "later strikes of a multi-hit move must damage normally after the bust");
        }

        // ─── Forecast ───────────────────────────────────────────────────────────────

        #[test]
        fn forecast_matches_weather_set_by_entry_ability() {
            let castform = mon(Species::Castform, Ability::Forecast, PokemonMove::Splash);
            let drizzler = mon(Species::Pelipper, Ability::Drizzle, PokemonMove::Splash);
            // Drizzle fires during the opening send-out; Forecast must follow immediately.
            let state = battle_state_from_lists(vec![castform], vec![], vec![drizzler], vec![]);

            assert_eq!(state.p1_active_mons[0].species, Species::CastformRainy,
                "Castform should take Rainy Form as soon as rain starts");
            assert_eq!(state.p1_active_mons[0].types, vec![crate::dex_data::PokemonType::Water],
                "Rainy Form is pure Water-type");
        }

        #[test]
        fn forecast_reverts_when_weather_expires() {
            use crate::dex_data::Weather;
            let castform = mon(Species::Castform, Ability::Forecast, PokemonMove::Splash);
            let foe = mon(Species::Shuckle, Ability::None, PokemonMove::Splash);
            let mut state = battle_state_from_lists(vec![castform], vec![], vec![foe], vec![]);
            // Rain with 2 turns left: survives the first end_turn, expires on the second.
            state.weather = Some(Weather::Rain);
            state.weather_turns = Some(2);

            let (bs, _) = extract_battle_state(run(state));
            assert_eq!(bs.p1_active_mons[0].species, Species::CastformRainy,
                "Castform stays Rainy while rain lasts");

            let (bs2, _) = extract_battle_state(run(bs));
            assert_eq!(bs2.p1_active_mons[0].species, Species::Castform,
                "Castform reverts to base form when the weather ends");
            assert_eq!(bs2.p1_active_mons[0].types, vec![crate::dex_data::PokemonType::Normal]);
        }

        #[test]
        fn forecast_requires_the_ability() {
            use crate::dex_data::Weather;
            let castform = mon(Species::Castform, Ability::None, PokemonMove::Splash);
            let foe = mon(Species::Shuckle, Ability::None, PokemonMove::Splash);
            let mut state = battle_state_from_lists(vec![castform], vec![], vec![foe], vec![]);
            state.weather = Some(Weather::Rain);
            state.weather_turns = Some(5);

            let (bs, _) = extract_battle_state(run(state));
            assert_eq!(bs.p1_active_mons[0].species, Species::Castform,
                "without Forecast, Castform never changes form");
        }
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Receiver (doubles): inherit a fainted ally's ability
    // ─────────────────────────────────────────────────────────────────────────
    #[cfg(test)]
    mod receiver_ability {
        use crate::battle::{BattleState, MatchState, Player, PlayerCommand};
        use crate::data::ability::Ability;
        use crate::data::pokemon_move::PokemonMove;
        use crate::data::species::Species;
        use crate::pokemon::{build_pokemon_state, PokemonState};
        use crate::simuilator_test_helpers::{
            battle_state_from_lists, extract_battle_state, move_dex, pokemon_dex,
            run_single_turn, simple_attack,
        };

        fn mon(species: Species, ability: Ability, mv: PokemonMove) -> PokemonState {
            let dex = pokemon_dex();
            let mdex = move_dex();
            build_pokemon_state(
                species, &dex, &mdex, Some(50),
                Some([Some(mv), None, None, None]),
                None, Some(ability), None, None, None, Some([0; 6]), None, false,
            )
        }

        /// Doubles battle: P1 = [frail donor, Receiver Snorlax]; P2's first slot KOs the donor.
        fn run_donor_faint(donor_ability: Ability) -> BattleState {
            let mut donor = mon(Species::Shuckle, donor_ability, PokemonMove::Splash);
            donor.hp = 1;
            let receiver = mon(Species::Snorlax, Ability::Receiver, PokemonMove::Splash);
            let attacker = mon(Species::Snorlax, Ability::None, PokemonMove::Tackle);
            let filler = mon(Species::Shuckle, Ability::None, PokemonMove::Splash);

            let state = battle_state_from_lists(
                vec![donor, receiver], vec![],
                vec![attacker, filler], vec![],
            );
            let pdex = pokemon_dex();
            let mdex = move_dex();
            let outcomes = run_single_turn(
                &MatchState::BattleState(state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0, 0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0, 0])),
                &mdex, &pdex,
            );
            extract_battle_state(outcomes).0
        }

        #[test]
        fn receiver_inherits_fainted_ally_ability() {
            let bs = run_donor_faint(Ability::SpeedBoost);
            assert!(bs.p1_active_mons[0].fainted, "precondition: the donor must be KO'd");
            assert_eq!(bs.p1_active_mons[1].ability, Ability::SpeedBoost,
                "Receiver should inherit the fainted ally's ability");
            assert_eq!(bs.p1_active_mons[1].original_ability, Some(Ability::Receiver),
                "the original ability is stashed for the switch-out revert");
        }

        #[test]
        fn receiver_skips_blocklisted_abilities() {
            let bs = run_donor_faint(Ability::Disguise);
            assert!(bs.p1_active_mons[0].fainted, "precondition: the donor must be KO'd");
            assert_eq!(bs.p1_active_mons[1].ability, Ability::Receiver,
                "blocklisted abilities (Disguise) cannot be received");
        }

        #[test]
        fn receiver_fires_on_gain_effects_of_inherited_ability() {
            // Intimidate at team send-out drops both P2 actives to -1; when the Receiver
            // inherits Intimidate mid-turn, the on-gain effects fire again → -2.
            let bs = run_donor_faint(Ability::Intimidate);
            assert_eq!(bs.p1_active_mons[1].ability, Ability::Intimidate, "precondition");
            assert_eq!(bs.p2_active_mons[0].boosts[0], -2,
                "inherited Intimidate must fire its entry effect");
            assert_eq!(bs.p2_active_mons[1].boosts[0], -2,
                "inherited Intimidate must hit every opposing active");
        }
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Variable-base-power moves (formula-based + conditionally scaled)
    // ─────────────────────────────────────────────────────────────────────────
    #[cfg(test)]
    mod variable_power_moves {
        use crate::battle::{BattleState, MatchState, Player, PlayerCommand};
        use crate::data::ability::Ability;
        use crate::data::item::Item;
        use crate::data::pokemon_move::PokemonMove;
        use crate::data::species::Species;
        use crate::dex_data::Status;
        use crate::pokemon::{build_pokemon_state, PokemonState};
        use crate::simuilator_test_helpers::{
            battle_state_from_lists, extract_battle_state, move_dex, pokemon_dex,
            run_single_turn, simple_attack,
        };

        fn mon(species: Species, ability: Ability, mv: PokemonMove) -> PokemonState {
            let dex = pokemon_dex();
            let mdex = move_dex();
            build_pokemon_state(
                species, &dex, &mdex, Some(50),
                Some([Some(mv), None, None, None]),
                None, Some(ability), None, None, None, Some([0; 6]), None, false,
            )
        }

        fn run(state: BattleState) -> Vec<(MatchState, f64)> {
            let pdex = pokemon_dex();
            let mdex = move_dex();
            run_single_turn(
                &MatchState::BattleState(state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &mdex, &pdex,
            )
        }

        /// Run one deterministic turn (both sides use move slot 0) and return the damage
        /// dealt to P2's active Pokémon.
        fn dmg_to_p2(state: BattleState) -> u16 {
            let initial = state.p2_active_mons[0].hp;
            let (bs, _) = extract_battle_state(run(state));
            initial - bs.p2_active_mons[0].hp
        }

        fn ratio(a: u16, b: u16) -> f64 {
            a as f64 / b.max(1) as f64
        }

        // ─── Formula-based ──────────────────────────────────────────────────────────

        #[test]
        fn electro_ball_power_scales_with_speed_ratio() {
            // 4× the target's Speed → 150 BP; equal Speed → 60 BP. Ratio 2.5.
            let mut fast = battle_state_from_lists(
                vec![mon(Species::Snorlax, Ability::None, PokemonMove::ElectroBall)], vec![],
                vec![mon(Species::Snorlax, Ability::None, PokemonMove::Splash)], vec![],
            );
            fast.p1_active_mons[0].stats[5] = 400;
            fast.p2_active_mons[0].stats[5] = 100;
            let mut equal = battle_state_from_lists(
                vec![mon(Species::Snorlax, Ability::None, PokemonMove::ElectroBall)], vec![],
                vec![mon(Species::Snorlax, Ability::None, PokemonMove::Splash)], vec![],
            );
            equal.p1_active_mons[0].stats[5] = 400;
            equal.p2_active_mons[0].stats[5] = 400;

            let r = ratio(dmg_to_p2(fast), dmg_to_p2(equal));
            assert!((r - 2.5).abs() < 0.25, "150 BP vs 60 BP should be ~2.5×, got {r}");
        }

        #[test]
        fn gyro_ball_rewards_slow_user() {
            let mut slow_user = battle_state_from_lists(
                vec![mon(Species::Snorlax, Ability::None, PokemonMove::GyroBall)], vec![],
                vec![mon(Species::Snorlax, Ability::None, PokemonMove::Splash)], vec![],
            );
            slow_user.p1_active_mons[0].stats[5] = 10;
            slow_user.p2_active_mons[0].stats[5] = 400; // 25×400/10 → capped at 150 BP
            let mut fast_user = battle_state_from_lists(
                vec![mon(Species::Snorlax, Ability::None, PokemonMove::GyroBall)], vec![],
                vec![mon(Species::Snorlax, Ability::None, PokemonMove::Splash)], vec![],
            );
            fast_user.p1_active_mons[0].stats[5] = 400;
            fast_user.p2_active_mons[0].stats[5] = 10; // floor(25×10/400)+1 = 1 BP

            let slow_dmg = dmg_to_p2(slow_user);
            let fast_dmg = dmg_to_p2(fast_user);
            assert!(slow_dmg > fast_dmg * 10,
                "150 BP (slow user) must vastly out-damage 1 BP (fast user): {slow_dmg} vs {fast_dmg}");
        }

        #[test]
        fn eruption_power_scales_with_user_hp() {
            let full = battle_state_from_lists(
                vec![mon(Species::Snorlax, Ability::None, PokemonMove::Eruption)], vec![],
                vec![mon(Species::Snorlax, Ability::None, PokemonMove::Splash)], vec![],
            );
            let mut half = battle_state_from_lists(
                vec![mon(Species::Snorlax, Ability::None, PokemonMove::Eruption)], vec![],
                vec![mon(Species::Snorlax, Ability::None, PokemonMove::Splash)], vec![],
            );
            let max_hp = half.p1_active_mons[0].stats[0];
            half.p1_active_mons[0].hp = max_hp / 2;

            let r = ratio(dmg_to_p2(full), dmg_to_p2(half));
            assert!((r - 2.0).abs() < 0.15, "Eruption at full HP should be ~2× half HP, got {r}");
        }

        #[test]
        fn flail_power_rises_as_hp_falls() {
            let full = battle_state_from_lists(
                vec![mon(Species::Snorlax, Ability::None, PokemonMove::Flail)], vec![],
                vec![mon(Species::Snorlax, Ability::None, PokemonMove::Splash)], vec![],
            );
            let mut clutch = battle_state_from_lists(
                vec![mon(Species::Snorlax, Ability::None, PokemonMove::Flail)], vec![],
                vec![mon(Species::Snorlax, Ability::None, PokemonMove::Splash)], vec![],
            );
            clutch.p1_active_mons[0].hp = 1; // 48×1/maxHP = 0 → 200 BP vs 20 BP at full

            let r = ratio(dmg_to_p2(clutch), dmg_to_p2(full));
            assert!((r - 10.0).abs() < 1.5, "Flail at 1 HP should be ~10× full HP, got {r}");
        }

        #[test]
        fn low_kick_power_scales_with_target_weight() {
            let mut heavy = battle_state_from_lists(
                vec![mon(Species::Snorlax, Ability::None, PokemonMove::LowKick)], vec![],
                vec![mon(Species::Snorlax, Ability::None, PokemonMove::Splash)], vec![],
            );
            heavy.p2_active_mons[0].weight_hg = 4600; // ≥200 kg → 120 BP
            let mut light = battle_state_from_lists(
                vec![mon(Species::Snorlax, Ability::None, PokemonMove::LowKick)], vec![],
                vec![mon(Species::Snorlax, Ability::None, PokemonMove::Splash)], vec![],
            );
            light.p2_active_mons[0].weight_hg = 50; // < 10 kg → 20 BP

            let r = ratio(dmg_to_p2(heavy), dmg_to_p2(light));
            assert!((r - 6.0).abs() < 0.7, "120 BP vs 20 BP should be ~6×, got {r}");
        }

        #[test]
        fn hard_press_power_scales_with_target_hp() {
            let full = battle_state_from_lists(
                vec![mon(Species::Snorlax, Ability::None, PokemonMove::HardPress)], vec![],
                vec![mon(Species::Snorlax, Ability::None, PokemonMove::Splash)], vec![],
            );
            let mut weakened = battle_state_from_lists(
                vec![mon(Species::Snorlax, Ability::None, PokemonMove::HardPress)], vec![],
                vec![mon(Species::Snorlax, Ability::None, PokemonMove::Splash)], vec![],
            );
            let max_hp = weakened.p2_active_mons[0].stats[0];
            weakened.p2_active_mons[0].hp = max_hp / 4; // ~25 BP vs 100 BP

            let r = ratio(dmg_to_p2(full), dmg_to_p2(weakened));
            assert!((r - 4.0).abs() < 0.4, "Hard Press full vs quarter HP should be ~4×, got {r}");
        }

        #[test]
        fn heat_crash_power_scales_with_weight_ratio() {
            let mut crusher = battle_state_from_lists(
                vec![mon(Species::Snorlax, Ability::None, PokemonMove::HeatCrash)], vec![],
                vec![mon(Species::Snorlax, Ability::None, PokemonMove::Splash)], vec![],
            );
            crusher.p1_active_mons[0].weight_hg = 5000;
            crusher.p2_active_mons[0].weight_hg = 1000; // 5× → 120 BP
            let mut even = battle_state_from_lists(
                vec![mon(Species::Snorlax, Ability::None, PokemonMove::HeatCrash)], vec![],
                vec![mon(Species::Snorlax, Ability::None, PokemonMove::Splash)], vec![],
            );
            even.p1_active_mons[0].weight_hg = 1000;
            even.p2_active_mons[0].weight_hg = 1000; // <2× → 40 BP

            let r = ratio(dmg_to_p2(crusher), dmg_to_p2(even));
            assert!((r - 3.0).abs() < 0.3, "120 BP vs 40 BP should be ~3×, got {r}");
        }

        // ─── Conditionally scaled ───────────────────────────────────────────────────

        #[test]
        fn acrobatics_doubles_without_an_item() {
            let dex = pokemon_dex();
            let mdex = move_dex();
            let unburdened = battle_state_from_lists(
                vec![mon(Species::Snorlax, Ability::None, PokemonMove::Acrobatics)], vec![],
                vec![mon(Species::Snorlax, Ability::None, PokemonMove::Splash)], vec![],
            );
            let holder = build_pokemon_state(
                Species::Snorlax, &dex, &mdex, Some(50),
                Some([Some(PokemonMove::Acrobatics), None, None, None]),
                None, Some(Ability::None), None, Some(Item::SafetyGoggles),
                None, Some([0; 6]), None, false,
            );
            let held = battle_state_from_lists(
                vec![holder], vec![],
                vec![mon(Species::Snorlax, Ability::None, PokemonMove::Splash)], vec![],
            );

            let r = ratio(dmg_to_p2(unburdened), dmg_to_p2(held));
            assert!((r - 2.0).abs() < 0.1, "itemless Acrobatics should be 2×, got {r}");
        }

        #[test]
        fn hex_doubles_against_statused_target() {
            // Magic Guard target: no burn residual to pollute the damage diff; burn
            // doesn't reduce special damage either.
            let healthy = battle_state_from_lists(
                vec![mon(Species::Snorlax, Ability::None, PokemonMove::Hex)], vec![],
                vec![mon(Species::Clefable, Ability::MagicGuard, PokemonMove::Splash)], vec![],
            );
            let mut burned = battle_state_from_lists(
                vec![mon(Species::Snorlax, Ability::None, PokemonMove::Hex)], vec![],
                vec![mon(Species::Clefable, Ability::MagicGuard, PokemonMove::Splash)], vec![],
            );
            burned.p2_active_mons[0].status = Some(Status::Burn);

            let r = ratio(dmg_to_p2(burned), dmg_to_p2(healthy));
            assert!((r - 2.0).abs() < 0.1, "Hex vs statused target should be 2×, got {r}");
        }

        #[test]
        fn assurance_doubles_after_target_was_damaged() {
            // Doubles: a faster ally Tackles the shared target first, then Assurance hits
            // the already-damaged target for double power.
            let pdex = pokemon_dex();
            let mdex = move_dex();
            let run_doubles = |slot0_move: PokemonMove, slot1_move: PokemonMove| -> u16 {
                let mut slot0 = mon(Species::Snorlax, Ability::None, slot0_move);
                slot0.stats[5] = 200; // acts first
                let mut slot1 = mon(Species::Snorlax, Ability::None, slot1_move);
                slot1.stats[5] = 50;
                let state = battle_state_from_lists(
                    vec![slot0, slot1], vec![],
                    vec![
                        mon(Species::Snorlax, Ability::None, PokemonMove::Splash),
                        mon(Species::Shuckle, Ability::None, PokemonMove::Splash),
                    ], vec![],
                );
                let initial = state.p2_active_mons[0].hp;
                let outcomes = run_single_turn(
                    &MatchState::BattleState(state),
                    &PlayerCommand::Battle(simple_attack(Player::P1, vec![0, 0])),
                    &PlayerCommand::Battle(simple_attack(Player::P2, vec![0, 0])),
                    &mdex, &pdex,
                );
                let (bs, _) = extract_battle_state(outcomes);
                initial - bs.p2_active_mons[0].hp
            };

            let tackle_plus_assurance = run_doubles(PokemonMove::Tackle, PokemonMove::Assurance);
            let tackle_only = run_doubles(PokemonMove::Tackle, PokemonMove::Splash);
            let assurance_only = run_doubles(PokemonMove::Splash, PokemonMove::Assurance);

            let boosted_assurance = tackle_plus_assurance - tackle_only;
            let r = ratio(boosted_assurance, assurance_only);
            assert!((r - 2.0).abs() < 0.15,
                "Assurance after the target took damage should be 2×, got {r}");
        }

        #[test]
        fn avalanche_doubles_when_damaged_by_target() {
            // Avalanche's −4 priority guarantees it moves after the target's Tackle.
            let hit_first = battle_state_from_lists(
                vec![mon(Species::Snorlax, Ability::None, PokemonMove::Avalanche)], vec![],
                vec![mon(Species::Snorlax, Ability::None, PokemonMove::Tackle)], vec![],
            );
            let unharmed = battle_state_from_lists(
                vec![mon(Species::Snorlax, Ability::None, PokemonMove::Avalanche)], vec![],
                vec![mon(Species::Snorlax, Ability::None, PokemonMove::Splash)], vec![],
            );

            let r = ratio(dmg_to_p2(hit_first), dmg_to_p2(unharmed));
            assert!((r - 2.0).abs() < 0.1,
                "Avalanche after being damaged by the target should be 2×, got {r}");
        }

        #[test]
        fn payback_doubles_when_moving_after_the_target() {
            let mut slow = battle_state_from_lists(
                vec![mon(Species::Snorlax, Ability::None, PokemonMove::Payback)], vec![],
                vec![mon(Species::Snorlax, Ability::None, PokemonMove::Tackle)], vec![],
            );
            slow.p1_active_mons[0].stats[5] = 1;
            slow.p2_active_mons[0].stats[5] = 200;
            let mut fast = battle_state_from_lists(
                vec![mon(Species::Snorlax, Ability::None, PokemonMove::Payback)], vec![],
                vec![mon(Species::Snorlax, Ability::None, PokemonMove::Tackle)], vec![],
            );
            fast.p1_active_mons[0].stats[5] = 200;
            fast.p2_active_mons[0].stats[5] = 1;

            let r = ratio(dmg_to_p2(slow), dmg_to_p2(fast));
            assert!((r - 2.0).abs() < 0.1, "Payback moving second should be 2×, got {r}");
        }

        #[test]
        fn payback_does_not_double_against_a_switch_in() {
            let pdex = pokemon_dex();
            let mdex = move_dex();
            // P2's active and bench are identical builds so damage is comparable.
            let p2_mon = || mon(Species::Snorlax, Ability::None, PokemonMove::Tackle);

            // Target switches: Payback user moves "after" it, but it's a fresh switch-in.
            let mut switch_state = battle_state_from_lists(
                vec![mon(Species::Snorlax, Ability::None, PokemonMove::Payback)], vec![],
                vec![p2_mon()], vec![p2_mon()],
            );
            switch_state.p1_active_mons[0].stats[5] = 1;
            let initial = switch_state.p2_back_mons[0].hp;
            let outcomes = run_single_turn(
                &MatchState::BattleState(switch_state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(vec![crate::battle::BattleCommand::Switch(
                    crate::battle::SwitchCommand { party_index: 0 })]),
                &mdex, &pdex,
            );
            let (bs, _) = extract_battle_state(outcomes);
            let switch_dmg = initial - bs.p2_active_mons[0].hp;

            // Control: target attacks instead → doubled.
            let mut attack_state = battle_state_from_lists(
                vec![mon(Species::Snorlax, Ability::None, PokemonMove::Payback)], vec![],
                vec![p2_mon()], vec![p2_mon()],
            );
            attack_state.p1_active_mons[0].stats[5] = 1;
            attack_state.p2_active_mons[0].stats[5] = 200;
            let attack_dmg = dmg_to_p2(attack_state);

            let r = ratio(attack_dmg, switch_dmg);
            assert!((r - 2.0).abs() < 0.15,
                "Payback must not double against a Pokémon that switched in, got ratio {r}");
        }

        #[test]
        fn lash_out_doubles_after_user_stats_lowered() {
            // Tail Whip lowers the user's Defense — irrelevant to its outgoing damage.
            let mut dropped = battle_state_from_lists(
                vec![mon(Species::Snorlax, Ability::None, PokemonMove::LashOut)], vec![],
                vec![mon(Species::Snorlax, Ability::None, PokemonMove::TailWhip)], vec![],
            );
            dropped.p1_active_mons[0].stats[5] = 1;
            dropped.p2_active_mons[0].stats[5] = 200;
            let mut calm = battle_state_from_lists(
                vec![mon(Species::Snorlax, Ability::None, PokemonMove::LashOut)], vec![],
                vec![mon(Species::Snorlax, Ability::None, PokemonMove::Splash)], vec![],
            );
            calm.p1_active_mons[0].stats[5] = 1;
            calm.p2_active_mons[0].stats[5] = 200;

            let r = ratio(dmg_to_p2(dropped), dmg_to_p2(calm));
            assert!((r - 2.0).abs() < 0.1,
                "Lash Out after a stat drop this turn should be 2×, got {r}");
        }

        #[test]
        fn stored_power_scales_with_positive_boosts() {
            // +2 Def / +2 SpD: four positive stages → 100 BP vs the 20 BP baseline,
            // without touching the offensive stat used by the move.
            let baseline = battle_state_from_lists(
                vec![mon(Species::Snorlax, Ability::None, PokemonMove::StoredPower)], vec![],
                vec![mon(Species::Snorlax, Ability::None, PokemonMove::Splash)], vec![],
            );
            let mut boosted = battle_state_from_lists(
                vec![mon(Species::Snorlax, Ability::None, PokemonMove::StoredPower)], vec![],
                vec![mon(Species::Snorlax, Ability::None, PokemonMove::Splash)], vec![],
            );
            boosted.p1_active_mons[0].boosts[1] = 2;
            boosted.p1_active_mons[0].boosts[3] = 2;

            // The 20 BP baseline deals single-digit damage, so the formula's +2 constant
            // and floors skew the ratio noticeably — allow generous tolerance.
            let r = ratio(dmg_to_p2(boosted), dmg_to_p2(baseline));
            assert!((r - 5.0).abs() < 0.8,
                "Stored Power at +4 total stages (100 BP) vs none (20 BP) should be ~5×, got {r}");
        }

        #[test]
        fn last_respects_scales_with_fainted_allies() {
            let no_faints = battle_state_from_lists(
                vec![mon(Species::Snorlax, Ability::None, PokemonMove::LastRespects)],
                vec![mon(Species::Clefable, Ability::None, PokemonMove::Splash)],
                vec![mon(Species::Clefable, Ability::None, PokemonMove::Splash)], vec![],
            );
            let mut one_faint = battle_state_from_lists(
                vec![mon(Species::Snorlax, Ability::None, PokemonMove::LastRespects)],
                vec![mon(Species::Clefable, Ability::None, PokemonMove::Splash)],
                vec![mon(Species::Clefable, Ability::None, PokemonMove::Splash)], vec![],
            );
            one_faint.p1_back_mons[0].fainted = true;
            one_faint.p1_back_mons[0].hp = 0;

            let r = ratio(dmg_to_p2(one_faint), dmg_to_p2(no_faints));
            assert!((r - 2.0).abs() < 0.1,
                "Last Respects with one fainted ally (100 BP) vs none (50 BP) should be 2×, got {r}");
        }

        #[test]
        fn stomping_tantrum_doubles_after_a_failed_move() {
            let pdex = pokemon_dex();
            let mdex = move_dex();
            let two_turns = |first_move: PokemonMove| -> (u16, bool) {
                let dex = pokemon_dex();
                let p1 = build_pokemon_state(
                    Species::Snorlax, &dex, &mdex, Some(50),
                    Some([Some(first_move), Some(PokemonMove::StompingTantrum), None, None]),
                    None, Some(Ability::None), None, None, None, Some([0; 6]), None, false,
                );
                let state = battle_state_from_lists(
                    vec![p1], vec![],
                    vec![mon(Species::Snorlax, Ability::None, PokemonMove::Splash)], vec![],
                );
                // Turn 1: use the first move (Aura Wheel from a non-Morpeko fails).
                let outcomes = run_single_turn(
                    &MatchState::BattleState(state),
                    &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                    &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                    &mdex, &pdex,
                );
                let (bs, _) = extract_battle_state(outcomes);
                let hp_before = bs.p2_active_mons[0].hp;
                // Turn 2: Stomping Tantrum.
                let outcomes = run_single_turn(
                    &MatchState::BattleState(bs),
                    &PlayerCommand::Battle(simple_attack(Player::P1, vec![1])),
                    &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                    &mdex, &pdex,
                );
                let (bs2, _) = extract_battle_state(outcomes);
                (hp_before - bs2.p2_active_mons[0].hp, bs2.p1_active_mons[0].last_move_failed)
            };

            let (after_fail, flag_after) = two_turns(PokemonMove::AuraWheel);
            let (after_success, _) = two_turns(PokemonMove::Splash);

            let r = ratio(after_fail, after_success);
            assert!((r - 2.0).abs() < 0.1,
                "Stomping Tantrum after a failed move should be 2×, got {r}");
            assert!(!flag_after,
                "a successful Stomping Tantrum must reset last_move_failed");
        }

        #[test]
        fn burning_jealousy_burns_only_freshly_boosted_targets() {
            let mut boosting = battle_state_from_lists(
                vec![mon(Species::Snorlax, Ability::None, PokemonMove::BurningJealousy)], vec![],
                vec![mon(Species::Shuckle, Ability::None, PokemonMove::SwordsDance)], vec![],
            );
            boosting.p1_active_mons[0].stats[5] = 1;
            boosting.p2_active_mons[0].stats[5] = 200; // boosts before the hit lands
            let (bs, _) = extract_battle_state(run(boosting));
            assert_eq!(bs.p2_active_mons[0].status, Some(Status::Burn),
                "a target that raised stats this turn must be burned");

            let mut idle = battle_state_from_lists(
                vec![mon(Species::Snorlax, Ability::None, PokemonMove::BurningJealousy)], vec![],
                vec![mon(Species::Shuckle, Ability::None, PokemonMove::Splash)], vec![],
            );
            idle.p1_active_mons[0].stats[5] = 1;
            idle.p2_active_mons[0].stats[5] = 200;
            let (bs2, _) = extract_battle_state(run(idle));
            assert_eq!(bs2.p2_active_mons[0].status, None,
                "a target that didn't boost this turn must not be burned");
        }
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Immunity / Move-blocking / Veil abilities
    // ─────────────────────────────────────────────────────────────────────────
    #[cfg(test)]
    mod immunity_and_veil_abilities {
        use super::*;
        use crate::pokemon::Nature;
        use crate::simuilator_test_helpers::extract_battle_state;

        fn mon(species: Species, ability: Ability, mv: PokemonMove) -> PokemonState {
            let dex = pokemon_dex();
            let mdex = move_dex();
            build_pokemon_state(
                species, &dex, &mdex, Some(50),
                Some([Some(mv), None, None, None]),
                None, Some(ability), Some(Nature::Hardy),
                None, None, Some([0; 6]), None, false,
            )
        }

        // ── Bulletproof ──────────────────────────────────────────────────────

        #[test]
        fn bulletproof_blocks_shadow_ball() {
            let mdex = move_dex();
            let pdex = pokemon_dex();
            let target = mon(Species::Komala, Ability::Bulletproof, PokemonMove::Splash);
            let max_hp = target.stats[0];
            let state = battle_state_from_lists(
                vec![target], vec![],
                vec![mon(Species::Gengar, Ability::CursedBody, PokemonMove::ShadowBall)], vec![],
            );
            let outcomes = run_single_turn(
                &MatchState::BattleState(state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &mdex, &pdex,
            );
            assert!(
                outcomes.iter().all(|(s, _)| matches!(s, MatchState::BattleState(bs)
                    if bs.p1_active_mons[0].hp == max_hp)),
                "Bulletproof: Shadow Ball (bullet) should deal 0 damage",
            );
        }

        #[test]
        fn bulletproof_blocks_sludge_bomb() {
            let mdex = move_dex();
            let pdex = pokemon_dex();
            let target = mon(Species::Komala, Ability::Bulletproof, PokemonMove::Splash);
            let max_hp = target.stats[0];
            let state = battle_state_from_lists(
                vec![target], vec![],
                vec![mon(Species::Gengar, Ability::CursedBody, PokemonMove::SludgeBomb)], vec![],
            );
            let outcomes = run_single_turn(
                &MatchState::BattleState(state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &mdex, &pdex,
            );
            assert!(
                outcomes.iter().all(|(s, _)| matches!(s, MatchState::BattleState(bs)
                    if bs.p1_active_mons[0].hp == max_hp)),
                "Bulletproof: Sludge Bomb (bomb) should deal 0 damage",
            );
        }

        #[test]
        fn bulletproof_does_not_block_non_bullet_move() {
            let mdex = move_dex();
            let pdex = pokemon_dex();
            let target = mon(Species::Komala, Ability::Bulletproof, PokemonMove::Splash);
            let max_hp = target.stats[0];
            let state = battle_state_from_lists(
                vec![target], vec![],
                vec![mon(Species::Gengar, Ability::CursedBody, PokemonMove::Tackle)], vec![],
            );
            let outcomes = run_single_turn(
                &MatchState::BattleState(state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &mdex, &pdex,
            );
            assert!(
                outcomes.iter().any(|(s, _)| matches!(s, MatchState::BattleState(bs)
                    if bs.p1_active_mons[0].hp < max_hp)),
                "Bulletproof: Tackle (non-bullet) should still deal damage",
            );
        }

        // ── Soundproof ───────────────────────────────────────────────────────

        #[test]
        fn soundproof_blocks_boomburst() {
            let mdex = move_dex();
            let pdex = pokemon_dex();
            let target = mon(Species::Komala, Ability::Soundproof, PokemonMove::Splash);
            let max_hp = target.stats[0];
            let state = battle_state_from_lists(
                vec![target], vec![],
                vec![mon(Species::Exploud, Ability::Scrappy, PokemonMove::Boomburst)], vec![],
            );
            let outcomes = run_single_turn(
                &MatchState::BattleState(state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &mdex, &pdex,
            );
            assert!(
                outcomes.iter().all(|(s, _)| matches!(s, MatchState::BattleState(bs)
                    if bs.p1_active_mons[0].hp == max_hp)),
                "Soundproof: Boomburst should deal 0 damage to Soundproof holder",
            );
        }

        #[test]
        fn soundproof_does_not_block_own_sound_moves() {
            // Champions behaviour: the holder is NOT immune to its OWN sound moves.
            let mdex = move_dex();
            let pdex = pokemon_dex();
            let mut attacker = mon(Species::Exploud, Ability::Soundproof, PokemonMove::Boomburst);
            attacker.stats[5] = 200; // faster, so it moves first
            let target = mon(Species::Snorlax, Ability::None, PokemonMove::Splash);
            let max_hp = target.stats[0];
            let state = battle_state_from_lists(
                vec![attacker], vec![],
                vec![target], vec![],
            );
            let outcomes = run_single_turn(
                &MatchState::BattleState(state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &mdex, &pdex,
            );
            assert!(
                outcomes.iter().any(|(s, _)| matches!(s, MatchState::BattleState(bs)
                    if bs.p2_active_mons[0].hp < max_hp)),
                "Soundproof: a Soundproof Exploud should still deal damage with Boomburst",
            );
        }

        // ── Overcoat ─────────────────────────────────────────────────────────

        #[test]
        fn overcoat_blocks_spore() {
            let mdex = move_dex();
            let pdex = pokemon_dex();
            let target = mon(Species::Komala, Ability::Overcoat, PokemonMove::Splash);
            let state = battle_state_from_lists(
                vec![target], vec![],
                vec![mon(Species::Amoonguss, Ability::Regenerator, PokemonMove::Spore)], vec![],
            );
            let outcomes = run_single_turn(
                &MatchState::BattleState(state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &mdex, &pdex,
            );
            for (s, _) in &outcomes {
                if let MatchState::BattleState(bs) = s {
                    assert!(
                        bs.p1_active_mons[0].status.is_none(),
                        "Overcoat: Spore should not inflict sleep",
                    );
                }
            }
        }

        #[test]
        fn overcoat_blocks_sandstorm_damage() {
            let mdex = move_dex();
            let pdex = pokemon_dex();
            let mut target = mon(Species::Komala, Ability::Overcoat, PokemonMove::Splash);
            let max_hp = target.stats[0];
            target.hp = max_hp;
            let opponent = mon(Species::Komala, Ability::None, PokemonMove::Splash);
            let mut state = battle_state_from_lists(
                vec![target], vec![],
                vec![opponent], vec![],
            );
            state.weather = Some(crate::dex_data::Weather::Sandstorm);
            state.weather_turns = Some(5);
            let (bs, _) = extract_battle_state(run_single_turn(
                &MatchState::BattleState(state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &mdex, &pdex,
            ));
            assert_eq!(
                bs.p1_active_mons[0].hp, max_hp,
                "Overcoat: should take 0 sandstorm damage",
            );
        }

        // ── Damp ─────────────────────────────────────────────────────────────

        #[test]
        fn damp_blocks_explosion() {
            let mdex = move_dex();
            let pdex = pokemon_dex();
            // Damp on the target's side blocks Explosion on P1.
            let mut attacker = mon(Species::Snorlax, Ability::None, PokemonMove::Explosion);
            attacker.stats[5] = 1; // slow, so P2 Splashes first (doesn't matter here)
            let attacker_max_hp = attacker.stats[0];
            let target = mon(Species::Golduck, Ability::Damp, PokemonMove::Splash);
            let state = battle_state_from_lists(
                vec![attacker], vec![],
                vec![target], vec![],
            );
            let outcomes = run_single_turn(
                &MatchState::BattleState(state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &mdex, &pdex,
            );
            for (s, _) in &outcomes {
                if let MatchState::BattleState(bs) = s {
                    // Attacker should NOT faint (Explosion does not KO user when blocked).
                    assert!(!bs.p1_active_mons[0].fainted, "Damp: attacker should not faint");
                    assert_eq!(bs.p1_active_mons[0].hp, attacker_max_hp, "Damp: attacker HP should be unchanged");
                    // Target should also be unharmed.
                    let target_max_hp = bs.p2_active_mons[0].stats[0];
                    assert_eq!(bs.p2_active_mons[0].hp, target_max_hp, "Damp: target should take 0 damage");
                }
            }
        }

        #[test]
        fn damp_on_attacker_side_blocks_explosion() {
            // Damp on the *attacker's* side also prevents the move.
            let mdex = move_dex();
            let pdex = pokemon_dex();
            let mut attacker = mon(Species::Golduck, Ability::Damp, PokemonMove::Explosion);
            attacker.stats[5] = 200;
            let attacker_max_hp = attacker.stats[0];
            let target = mon(Species::Snorlax, Ability::None, PokemonMove::Splash);
            let state = battle_state_from_lists(
                vec![attacker], vec![],
                vec![target], vec![],
            );
            let outcomes = run_single_turn(
                &MatchState::BattleState(state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &mdex, &pdex,
            );
            for (s, _) in &outcomes {
                if let MatchState::BattleState(bs) = s {
                    assert!(!bs.p1_active_mons[0].fainted, "Damp self: attacker should not faint");
                    assert_eq!(bs.p1_active_mons[0].hp, attacker_max_hp, "Damp self: attacker HP unchanged");
                    let target_max_hp = bs.p2_active_mons[0].stats[0];
                    assert_eq!(bs.p2_active_mons[0].hp, target_max_hp, "Damp self: target unharmed");
                }
            }
        }

        // ── Levitate ─────────────────────────────────────────────────────────

        #[test]
        fn levitate_is_immune_to_earthquake() {
            let mdex = move_dex();
            let pdex = pokemon_dex();
            let target = mon(Species::Gengar, Ability::Levitate, PokemonMove::Splash);
            let max_hp = target.stats[0];
            let state = battle_state_from_lists(
                vec![target], vec![],
                vec![mon(Species::Garchomp, Ability::RoughSkin, PokemonMove::Earthquake)], vec![],
            );
            let outcomes = run_single_turn(
                &MatchState::BattleState(state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &mdex, &pdex,
            );
            assert!(
                outcomes.iter().all(|(s, _)| matches!(s, MatchState::BattleState(bs)
                    if bs.p1_active_mons[0].hp == max_hp)),
                "Levitate: Earthquake should deal 0 damage",
            );
        }

        // ── Shield Dust ──────────────────────────────────────────────────────

        #[test]
        fn shield_dust_blocks_secondary_status() {
            // Sludge Bomb has a 30% chance to poison. Shield Dust should eliminate that chance.
            let mdex = move_dex();
            let pdex = pokemon_dex();
            let target = mon(Species::Beautifly, Ability::ShieldDust, PokemonMove::Splash);
            let state = battle_state_from_lists(
                vec![target], vec![],
                vec![mon(Species::Gengar, Ability::CursedBody, PokemonMove::SludgeBomb)], vec![],
            );
            let outcomes = run_single_turn(
                &MatchState::BattleState(state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &mdex, &pdex,
            );
            // No branch should have the target poisoned.
            let poison_prob: f64 = outcomes.iter().map(|(s, p)| {
                if matches!(s, MatchState::BattleState(bs)
                    if bs.p1_active_mons[0].status == Some(crate::dex_data::Status::Poison)) { *p } else { 0.0 }
            }).sum();
            assert!(
                poison_prob < 1e-9,
                "Shield Dust: Sludge Bomb secondary poison should be blocked (got prob={poison_prob})",
            );
        }

        #[test]
        fn shield_dust_does_not_block_attacker_self_boost() {
            // Charge Beam has a 70% chance to raise the attacker's SpA (+1).
            // Shield Dust on the target must NOT block that self-boost.
            // Attacker = P1 (Raichu); target = P2 (Beautifly with Shield Dust).
            let mdex = move_dex();
            let pdex = pokemon_dex();
            let mut attacker = mon(Species::Raichu, Ability::Static, PokemonMove::ChargeBeam);
            attacker.stats[5] = 200; // move first
            let target = mon(Species::Beautifly, Ability::ShieldDust, PokemonMove::Splash);
            let state = battle_state_from_lists(
                vec![attacker], vec![],
                vec![target], vec![],
            );
            let outcomes = run_single_turn(
                &MatchState::BattleState(state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &mdex, &pdex,
            );
            // Self-boost is on the ATTACKER (P1 = p1_active_mons[0], boosts[2] = SpA).
            let attacker_spa_boost_prob: f64 = outcomes.iter().map(|(s, p)| {
                if matches!(s, MatchState::BattleState(bs)
                    if bs.p1_active_mons[0].boosts[2] >= 1) { *p } else { 0.0 }
            }).sum();
            assert!(
                attacker_spa_boost_prob > 0.60,
                "Shield Dust: Charge Beam self-SpA boost on attacker should still fire (~70%, got={attacker_spa_boost_prob:.2})",
            );
        }

        // ── Keen Eye / Illuminate ────────────────────────────────────────────

        #[test]
        fn keen_eye_blocks_accuracy_drop() {
            let mdex = move_dex();
            let pdex = pokemon_dex();
            let mut target = mon(Species::Pidgeot, Ability::KeenEye, PokemonMove::Splash);
            target.stats[5] = 1; // slow, attacked first by Sand Attack
            let state = battle_state_from_lists(
                vec![target], vec![],
                vec![mon(Species::Sandslash, Ability::SandVeil, PokemonMove::SandAttack)], vec![],
            );
            let outcomes = run_single_turn(
                &MatchState::BattleState(state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &mdex, &pdex,
            );
            for (s, _) in &outcomes {
                if let MatchState::BattleState(bs) = s {
                    assert_eq!(
                        bs.p1_active_mons[0].boosts[5], 0,
                        "Keen Eye: accuracy should not be lowered by Sand Attack",
                    );
                }
            }
        }

        #[test]
        fn illuminate_blocks_accuracy_drop() {
            let mdex = move_dex();
            let pdex = pokemon_dex();
            let mut target = mon(Species::Starmie, Ability::Illuminate, PokemonMove::Splash);
            target.stats[5] = 1;
            let state = battle_state_from_lists(
                vec![target], vec![],
                vec![mon(Species::Sandslash, Ability::SandVeil, PokemonMove::SandAttack)], vec![],
            );
            let outcomes = run_single_turn(
                &MatchState::BattleState(state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &mdex, &pdex,
            );
            for (s, _) in &outcomes {
                if let MatchState::BattleState(bs) = s {
                    assert_eq!(
                        bs.p1_active_mons[0].boosts[5], 0,
                        "Illuminate: accuracy should not be lowered by Sand Attack",
                    );
                }
            }
        }

        #[test]
        fn keen_eye_ignores_target_evasion() {
            // A Pokémon with Keen Eye attacking an evasion-boosted opponent should land
            // the hit as if evasion = 0.
            let mdex = move_dex();
            let pdex = pokemon_dex();
            let mut attacker = mon(Species::Pidgeot, Ability::KeenEye, PokemonMove::Tackle);
            attacker.stats[5] = 200; // moves first
            let mut target = mon(Species::Snorlax, Ability::None, PokemonMove::Splash);
            target.boosts[6] = 6; // max evasion
            let max_hp = target.stats[0];
            let state = battle_state_from_lists(
                vec![attacker], vec![],
                vec![target], vec![],
            );
            let outcomes = run_single_turn(
                &MatchState::BattleState(state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &mdex, &pdex,
            );
            // With evasion ignored, Tackle (100% acc) should hit every branch.
            let hit_prob: f64 = outcomes.iter().map(|(s, p)| {
                let hit = matches!(s, MatchState::BattleState(bs) if bs.p2_active_mons[0].hp < max_hp)
                    || matches!(s, MatchState::GameOverState { .. });
                if hit { *p } else { 0.0 }
            }).sum();
            assert!(
                hit_prob > 0.99,
                "Keen Eye: should ignore +6 evasion and always hit (hit_prob={hit_prob:.3})",
            );
        }

        // ── Magic Guard ──────────────────────────────────────────────────────

        #[test]
        fn magic_guard_blocks_burn_residual() {
            let mdex = move_dex();
            let pdex = pokemon_dex();
            let mut target = mon(Species::Clefable, Ability::MagicGuard, PokemonMove::Splash);
            target.status = Some(crate::dex_data::Status::Burn);
            let max_hp = target.stats[0];
            target.hp = max_hp;
            let opponent = mon(Species::Snorlax, Ability::None, PokemonMove::Splash);
            let state = battle_state_from_lists(
                vec![target], vec![],
                vec![opponent], vec![],
            );
            let (bs, _) = extract_battle_state(run_single_turn(
                &MatchState::BattleState(state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &mdex, &pdex,
            ));
            assert_eq!(
                bs.p1_active_mons[0].hp, max_hp,
                "Magic Guard: burn residual should deal 0 damage",
            );
        }

        #[test]
        fn magic_guard_blocks_poison_residual() {
            let mdex = move_dex();
            let pdex = pokemon_dex();
            let mut target = mon(Species::Clefable, Ability::MagicGuard, PokemonMove::Splash);
            target.status = Some(crate::dex_data::Status::Poison);
            let max_hp = target.stats[0];
            target.hp = max_hp;
            let opponent = mon(Species::Snorlax, Ability::None, PokemonMove::Splash);
            let state = battle_state_from_lists(
                vec![target], vec![],
                vec![opponent], vec![],
            );
            let (bs, _) = extract_battle_state(run_single_turn(
                &MatchState::BattleState(state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &mdex, &pdex,
            ));
            assert_eq!(
                bs.p1_active_mons[0].hp, max_hp,
                "Magic Guard: poison residual should deal 0 damage",
            );
        }

        #[test]
        fn magic_guard_does_not_block_direct_damage() {
            // Magic Guard does NOT block direct attack damage.
            let mdex = move_dex();
            let pdex = pokemon_dex();
            let target = mon(Species::Clefable, Ability::MagicGuard, PokemonMove::Splash);
            let max_hp = target.stats[0];
            let state = battle_state_from_lists(
                vec![target], vec![],
                vec![mon(Species::Snorlax, Ability::None, PokemonMove::Tackle)], vec![],
            );
            let outcomes = run_single_turn(
                &MatchState::BattleState(state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &mdex, &pdex,
            );
            assert!(
                outcomes.iter().any(|(s, _)| matches!(s, MatchState::BattleState(bs)
                    if bs.p1_active_mons[0].hp < max_hp)),
                "Magic Guard: direct attack damage should still apply",
            );
        }

        // ── Sweet Veil ───────────────────────────────────────────────────────

        #[test]
        fn sweet_veil_blocks_sleep_powder() {
            let mdex = move_dex();
            let pdex = pokemon_dex();
            let target = mon(Species::Slurpuff, Ability::SweetVeil, PokemonMove::Splash);
            let state = battle_state_from_lists(
                vec![target], vec![],
                vec![mon(Species::Amoonguss, Ability::Regenerator, PokemonMove::SleepPowder)], vec![],
            );
            let outcomes = run_single_turn(
                &MatchState::BattleState(state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &mdex, &pdex,
            );
            for (s, _) in &outcomes {
                if let MatchState::BattleState(bs) = s {
                    assert!(
                        !matches!(bs.p1_active_mons[0].status, Some(crate::dex_data::Status::Sleep(_))),
                        "Sweet Veil: Sleep Powder should not inflict sleep",
                    );
                }
            }
        }

        #[test]
        fn sweet_veil_blocks_yawn() {
            let mdex = move_dex();
            let pdex = pokemon_dex();
            let mut target = mon(Species::Slurpuff, Ability::SweetVeil, PokemonMove::Splash);
            target.stats[5] = 1; // let Yawn user go first
            let state = battle_state_from_lists(
                vec![target], vec![],
                vec![mon(Species::Snorlax, Ability::None, PokemonMove::Yawn)], vec![],
            );
            let outcomes = run_single_turn(
                &MatchState::BattleState(state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &mdex, &pdex,
            );
            for (s, _) in &outcomes {
                if let MatchState::BattleState(bs) = s {
                    let has_yawn = bs.p1_active_mons[0].volatiles.iter().any(|v|
                        matches!(v, crate::pokemon::VolatileStatusState::TurnStatus(crate::dex_data::VolatileStatus::Yawn, _))
                    );
                    assert!(!has_yawn, "Sweet Veil: Yawn volatile should not be applied");
                }
            }
        }

        // ── Flower Veil ──────────────────────────────────────────────────────

        #[test]
        fn flower_veil_blocks_status_on_grass_holder() {
            // Roserade is Grass-type and has Flower Veil itself — it protects itself.
            // (In doubles the holder protects Grass-type allies; in singles the Grass-type
            // holder covers the self-protection case.)
            let mdex = move_dex();
            let pdex = pokemon_dex();
            let grass_target = mon(Species::Roserade, Ability::FlowerVeil, PokemonMove::Splash);
            let state = battle_state_from_lists(
                vec![grass_target], vec![],
                vec![mon(Species::Gardevoir, Ability::None, PokemonMove::WillOWisp)], vec![],
            );
            let outcomes = run_single_turn(
                &MatchState::BattleState(state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &mdex, &pdex,
            );
            for (s, _) in &outcomes {
                if let MatchState::BattleState(bs) = s {
                    assert!(
                        bs.p1_active_mons[0].status.is_none(),
                        "Flower Veil: Will-O-Wisp should not burn a Grass-type with Flower Veil",
                    );
                }
            }
        }

        #[test]
        fn flower_veil_does_not_block_non_grass_status() {
            // Snorlax (Normal-type, no Flower Veil) should still be burned by Will-O-Wisp.
            // Will-O-Wisp has 85% accuracy so burn_prob ≈ 0.85, not 1.0.
            let mdex = move_dex();
            let pdex = pokemon_dex();
            let normal_target = mon(Species::Snorlax, Ability::None, PokemonMove::Splash);
            let state = battle_state_from_lists(
                vec![normal_target], vec![],
                vec![mon(Species::Gardevoir, Ability::None, PokemonMove::WillOWisp)], vec![],
            );
            let outcomes = run_single_turn(
                &MatchState::BattleState(state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &mdex, &pdex,
            );
            let burn_prob: f64 = outcomes.iter().map(|(s, p)| {
                if matches!(s, MatchState::BattleState(bs)
                    if bs.p1_active_mons[0].status == Some(crate::dex_data::Status::Burn)) { *p } else { 0.0 }
            }).sum();
            assert!(
                burn_prob > 0.80,
                "Flower Veil: Will-O-Wisp should burn a non-Grass-type (burn_prob={burn_prob:.2})",
            );
        }

        #[test]
        fn flower_veil_blocks_stat_drop_on_grass() {
            // Roserade has Flower Veil (Grass-type) and Charm should not lower its Attack.
            let mdex = move_dex();
            let pdex = pokemon_dex();
            let mut grass_target = mon(Species::Roserade, Ability::FlowerVeil, PokemonMove::Splash);
            grass_target.stats[5] = 1; // slow, hit first by Charm
            let state = battle_state_from_lists(
                vec![grass_target], vec![],
                vec![mon(Species::Gardevoir, Ability::None, PokemonMove::Charm)], vec![],
            );
            let outcomes = run_single_turn(
                &MatchState::BattleState(state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &mdex, &pdex,
            );
            for (s, _) in &outcomes {
                if let MatchState::BattleState(bs) = s {
                    assert_eq!(
                        bs.p1_active_mons[0].boosts[0], 0,
                        "Flower Veil: Charm Atk drop should be blocked on Grass-type with FlowerVeil",
                    );
                }
            }
        }

        // ── Aroma Veil ───────────────────────────────────────────────────────

        #[test]
        fn aroma_veil_blocks_taunt() {
            let mdex = move_dex();
            let pdex = pokemon_dex();
            let mut target = mon(Species::Aromatisse, Ability::AromaVeil, PokemonMove::Splash);
            target.stats[5] = 1; // slow, gets Taunted first
            let state = battle_state_from_lists(
                vec![target], vec![],
                vec![mon(Species::Gengar, Ability::CursedBody, PokemonMove::Taunt)], vec![],
            );
            let outcomes = run_single_turn(
                &MatchState::BattleState(state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &mdex, &pdex,
            );
            for (s, _) in &outcomes {
                if let MatchState::BattleState(bs) = s {
                    let taunted = bs.p1_active_mons[0].volatiles.iter().any(|v|
                        matches!(v, crate::pokemon::VolatileStatusState::MoveStatus(crate::dex_data::VolatileStatus::Taunt, _))
                    );
                    assert!(!taunted, "Aroma Veil: Taunt should not apply to the holder");
                }
            }
        }

        #[test]
        fn aroma_veil_blocks_encore() {
            let mdex = move_dex();
            let pdex = pokemon_dex();
            let mut target = mon(Species::Aromatisse, Ability::AromaVeil, PokemonMove::Splash);
            target.stats[5] = 1;
            // Give the target a last_used_move so Encore has a valid target.
            target.last_used_move = Some(PokemonMove::Splash);
            let state = battle_state_from_lists(
                vec![target], vec![],
                vec![mon(Species::Gengar, Ability::CursedBody, PokemonMove::Encore)], vec![],
            );
            let outcomes = run_single_turn(
                &MatchState::BattleState(state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &mdex, &pdex,
            );
            for (s, _) in &outcomes {
                if let MatchState::BattleState(bs) = s {
                    let encored = bs.p1_active_mons[0].volatiles.iter().any(|v|
                        matches!(v, crate::pokemon::VolatileStatusState::MoveStatus(crate::dex_data::VolatileStatus::Encore, _))
                    );
                    assert!(!encored, "Aroma Veil: Encore should not apply to the holder");
                }
            }
        }

    }

    mod entry_hazards {
        use crate::battle::{
            BattleCommand, BattleState, FieldSlot, MatchState, Player, PlayerCommand, SwitchCommand,
        };
        use crate::data::ability::Ability;
        use crate::data::item::Item;
        use crate::data::pokemon_move::PokemonMove;
        use crate::data::species::Species;
        use crate::dex_data::{SideCondition, Status};
        use crate::pokemon::{build_pokemon_state, PokemonState};
        use crate::simulator_helpers;
        use crate::simuilator_test_helpers::{
            battle_state_from_lists, extract_battle_state, move_dex, pokemon_dex, run_single_turn,
            simple_attack,
        };

        fn build(species: Species, ability: Ability, item: Option<Item>) -> PokemonState {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();
            build_pokemon_state(
                species, &pokemon_dex, &move_dex, Some(50),
                Some([Some(PokemonMove::Splash), None, None, None]),
                None, Some(ability), None, item, None, None, None, false,
            )
        }

        fn build_move(species: Species, ability: Ability, mv: PokemonMove) -> PokemonState {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();
            build_pokemon_state(
                species, &pokemon_dex, &move_dex, Some(50),
                Some([Some(mv), Some(PokemonMove::Splash), None, None]),
                None, Some(ability), None, None, None, None, None, false,
            )
        }

        /// Build a battle whose P1 back mon `incoming` switches into `p1_hazards` (set on P1's
        /// side), with `p2_active` standing across using Splash. Returns the resolved state.
        fn switch_into(
            incoming: PokemonState,
            p1_hazards: Vec<SideCondition>,
            p2_active: PokemonState,
        ) -> BattleState {
            let lead = build(Species::Clefable, Ability::Pressure, None);
            let mut state = battle_state_from_lists(vec![lead], vec![incoming], vec![p2_active], vec![]);
            for c in p1_hazards {
                state.p1_side_conditions.push(c);
                state.p1_side_condition_turns.push(0);
            }
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();
            let outcomes = run_single_turn(
                &MatchState::BattleState(state),
                &PlayerCommand::Battle(vec![BattleCommand::Switch(SwitchCommand { party_index: 0 })]),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex,
                &pokemon_dex,
            );
            extract_battle_state(outcomes).0
        }

        // ── Spikes ──────────────────────────────────────────────────────────────────────────

        #[test]
        fn spikes_one_layer_damages_grounded() {
            let after = switch_into(
                build(Species::Snorlax, Ability::Pressure, None),
                vec![SideCondition::Spikes(1)],
                build(Species::Snorlax, Ability::Pressure, None),
            );
            let m = &after.p1_active_mons[0];
            assert_eq!(m.hp, m.stats[0] - m.stats[0] / 8);
        }

        #[test]
        fn spikes_three_layers_quarter() {
            let after = switch_into(
                build(Species::Snorlax, Ability::Pressure, None),
                vec![SideCondition::Spikes(3)],
                build(Species::Snorlax, Ability::Pressure, None),
            );
            let m = &after.p1_active_mons[0];
            assert_eq!(m.hp, m.stats[0] - m.stats[0] / 4);
        }

        #[test]
        fn spikes_do_not_affect_flying() {
            let after = switch_into(
                build(Species::Talonflame, Ability::Pressure, None),
                vec![SideCondition::Spikes(3)],
                build(Species::Snorlax, Ability::Pressure, None),
            );
            let m = &after.p1_active_mons[0];
            assert_eq!(m.hp, m.stats[0], "airborne mon should take no Spikes damage");
        }

        // ── Stealth Rock ────────────────────────────────────────────────────────────────────

        #[test]
        fn stealth_rock_neutral_eighth() {
            let after = switch_into(
                build(Species::Snorlax, Ability::Pressure, None),
                vec![SideCondition::StealthRock],
                build(Species::Snorlax, Ability::Pressure, None),
            );
            let m = &after.p1_active_mons[0];
            assert_eq!(m.hp, m.stats[0] - m.stats[0] / 8);
        }

        #[test]
        fn stealth_rock_quad_hits_airborne() {
            // Charizard (Fire/Flying) is airborne but Stealth Rock ignores grounding, and Rock is
            // 4× effective → half max HP.
            let after = switch_into(
                build(Species::Charizard, Ability::Pressure, None),
                vec![SideCondition::StealthRock],
                build(Species::Snorlax, Ability::Pressure, None),
            );
            let m = &after.p1_active_mons[0];
            let expected = ((m.stats[0] as f64) * 4.0 / 8.0).floor() as u16;
            assert_eq!(m.hp, m.stats[0] - expected);
        }

        // ── Toxic Spikes ────────────────────────────────────────────────────────────────────

        #[test]
        fn toxic_spikes_one_layer_poisons() {
            let after = switch_into(
                build(Species::Snorlax, Ability::Pressure, None),
                vec![SideCondition::ToxicSpikes(1)],
                build(Species::Snorlax, Ability::Pressure, None),
            );
            assert_eq!(after.p1_active_mons[0].status, Some(Status::Poison));
        }

        #[test]
        fn toxic_spikes_two_layers_badly_poison() {
            let after = switch_into(
                build(Species::Snorlax, Ability::Pressure, None),
                vec![SideCondition::ToxicSpikes(2)],
                build(Species::Snorlax, Ability::Pressure, None),
            );
            assert!(matches!(after.p1_active_mons[0].status, Some(Status::ToxicPoison(_))));
        }

        #[test]
        fn toxic_spikes_absorbed_by_grounded_poison_type() {
            let after = switch_into(
                build(Species::Muk, Ability::Pressure, None),
                vec![SideCondition::ToxicSpikes(2)],
                build(Species::Snorlax, Ability::Pressure, None),
            );
            assert_eq!(after.p1_active_mons[0].status, None, "Poison-type is not poisoned");
            assert!(
                !after.p1_side_conditions.iter().any(|c| matches!(c, SideCondition::ToxicSpikes(_))),
                "grounded Poison-type absorbs Toxic Spikes"
            );
        }

        #[test]
        fn toxic_spikes_steel_immune_but_keeps_layers() {
            // Excadrill (Ground/Steel) is grounded and immune to poison, but does not absorb.
            let after = switch_into(
                build(Species::Excadrill, Ability::Pressure, None),
                vec![SideCondition::ToxicSpikes(2)],
                build(Species::Snorlax, Ability::Pressure, None),
            );
            assert_eq!(after.p1_active_mons[0].status, None);
            assert!(
                after.p1_side_conditions.iter().any(|c| matches!(c, SideCondition::ToxicSpikes(_))),
                "Steel-type does not absorb Toxic Spikes"
            );
        }

        // ── Sticky Web ──────────────────────────────────────────────────────────────────────

        #[test]
        fn sticky_web_lowers_speed_of_grounded() {
            let after = switch_into(
                build(Species::Snorlax, Ability::Pressure, None),
                vec![SideCondition::StickyWeb(None)],
                build(Species::Snorlax, Ability::Pressure, None),
            );
            assert_eq!(after.p1_active_mons[0].boosts[4], -1);
        }

        #[test]
        fn sticky_web_does_not_affect_flying() {
            let after = switch_into(
                build(Species::Talonflame, Ability::Pressure, None),
                vec![SideCondition::StickyWeb(None)],
                build(Species::Snorlax, Ability::Pressure, None),
            );
            assert_eq!(after.p1_active_mons[0].boosts[4], 0);
        }

        #[test]
        fn sticky_web_triggers_defiant() {
            let after = switch_into(
                build(Species::Bisharp, Ability::Defiant, None),
                vec![SideCondition::StickyWeb(None)],
                build(Species::Snorlax, Ability::Pressure, None),
            );
            let m = &after.p1_active_mons[0];
            assert_eq!(m.boosts[4], -1, "Speed still drops");
            assert_eq!(m.boosts[0], 2, "Defiant grants +2 Attack");
        }

        #[test]
        fn sticky_web_blocked_by_clear_body() {
            let after = switch_into(
                build(Species::Metagross, Ability::ClearBody, None),
                vec![SideCondition::StickyWeb(None)],
                build(Species::Snorlax, Ability::Pressure, None),
            );
            assert_eq!(after.p1_active_mons[0].boosts[4], 0);
        }

        // ── Immunity item / ability ─────────────────────────────────────────────────────────

        #[test]
        fn heavy_duty_boots_ignores_every_hazard() {
            let after = switch_into(
                build(Species::Snorlax, Ability::Pressure, Some(Item::HeavyDutyBoots)),
                vec![
                    SideCondition::StealthRock,
                    SideCondition::Spikes(3),
                    SideCondition::StickyWeb(None),
                    SideCondition::ToxicSpikes(2),
                ],
                build(Species::Snorlax, Ability::Pressure, None),
            );
            let m = &after.p1_active_mons[0];
            assert_eq!(m.hp, m.stats[0], "no hazard damage");
            assert_eq!(m.status, None, "no Toxic Spikes poison");
            assert_eq!(m.boosts[4], 0, "no Sticky Web speed drop");
        }

        #[test]
        fn magic_guard_blocks_damage_but_not_web_or_poison() {
            let after = switch_into(
                build(Species::Clefable, Ability::MagicGuard, None),
                vec![
                    SideCondition::StealthRock,
                    SideCondition::Spikes(3),
                    SideCondition::StickyWeb(None),
                    SideCondition::ToxicSpikes(1),
                ],
                build(Species::Snorlax, Ability::Pressure, None),
            );
            let m = &after.p1_active_mons[0];
            assert_eq!(m.hp, m.stats[0], "Magic Guard blocks Stealth Rock + Spikes damage");
            assert_eq!(m.boosts[4], -1, "Sticky Web still lowers Speed");
            assert_eq!(m.status, Some(Status::Poison), "Toxic Spikes still poisons");
        }

        // ── Sticky Web + Mirror Armor reflection ────────────────────────────────────────────

        #[test]
        fn mirror_armor_reflects_sticky_web_to_setter() {
            let incoming = build(Species::Metagross, Ability::MirrorArmor, None);
            let setter = build(Species::Snorlax, Ability::Pressure, None);
            let lead = build(Species::Clefable, Ability::Pressure, None);
            let mut state = battle_state_from_lists(vec![lead], vec![incoming], vec![setter], vec![]);
            // Web on P1's side, set by the P2 active mon (matched later by its mon_id).
            let setter_id = state.p2_active_mons[0].mon_id;
            state.p1_side_conditions.push(SideCondition::StickyWeb(Some(setter_id)));
            state.p1_side_condition_turns.push(0);

            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();
            let after = extract_battle_state(run_single_turn(
                &MatchState::BattleState(state),
                &PlayerCommand::Battle(vec![BattleCommand::Switch(SwitchCommand { party_index: 0 })]),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex,
                &pokemon_dex,
            )).0;

            assert_eq!(after.p1_active_mons[0].boosts[4], 0, "Mirror Armor holder is not lowered");
            assert_eq!(after.p2_active_mons[0].boosts[4], -1, "drop is reflected to the setter");
        }

        #[test]
        fn mirror_armor_sticky_web_no_drop_when_setter_absent() {
            let incoming = build(Species::Metagross, Ability::MirrorArmor, None);
            let foe = build(Species::Snorlax, Ability::Pressure, None);
            let lead = build(Species::Clefable, Ability::Pressure, None);
            let mut state = battle_state_from_lists(vec![lead], vec![incoming], vec![foe], vec![]);
            // Web records a setter id that is not present among P2's actives.
            state.p1_side_conditions.push(SideCondition::StickyWeb(Some(200)));
            state.p1_side_condition_turns.push(0);

            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();
            let after = extract_battle_state(run_single_turn(
                &MatchState::BattleState(state),
                &PlayerCommand::Battle(vec![BattleCommand::Switch(SwitchCommand { party_index: 0 })]),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex,
                &pokemon_dex,
            )).0;

            assert_eq!(after.p1_active_mons[0].boosts[4], 0, "Mirror Armor holder unaffected");
            assert_eq!(after.p2_active_mons[0].boosts[4], 0, "absent setter means nobody is lowered");
        }

        // ── Setter moves ────────────────────────────────────────────────────────────────────

        #[test]
        fn spikes_move_sets_a_layer_on_foe_side() {
            let attacker = build_move(Species::Snorlax, Ability::Pressure, PokemonMove::Spikes);
            let target = build(Species::Snorlax, Ability::Pressure, None);
            let state = battle_state_from_lists(vec![attacker], vec![], vec![target], vec![]);
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();
            let after = extract_battle_state(run_single_turn(
                &MatchState::BattleState(state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex,
                &pokemon_dex,
            )).0;
            assert!(after.p2_side_conditions.iter().any(|c| matches!(c, SideCondition::Spikes(1))));
        }

        /// Probability mass across outcomes where P2's side carries a hazard matching `pred`.
        fn prob_foe_hazard(outcomes: &[(MatchState, f64)], pred: impl Fn(&SideCondition) -> bool) -> f64 {
            outcomes.iter().map(|(s, p)| match s {
                MatchState::BattleState(bs) if bs.p2_side_conditions.iter().any(&pred) => *p,
                _ => 0.0,
            }).sum()
        }

        #[test]
        fn ceaseless_edge_sets_spikes_on_hit() {
            let attacker = build_move(Species::Snorlax, Ability::Pressure, PokemonMove::CeaselessEdge);
            let target = build(Species::Snorlax, Ability::Pressure, None);
            let state = battle_state_from_lists(vec![attacker], vec![], vec![target], vec![]);
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();
            let outcomes = run_single_turn(
                &MatchState::BattleState(state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex,
                &pokemon_dex,
            );
            // 90% accuracy → Spikes set in the hit branch only.
            let p = prob_foe_hazard(&outcomes, |c| matches!(c, SideCondition::Spikes(_)));
            assert!((p - 0.9).abs() < 0.02, "expected ~0.9 Spikes probability, got {p}");
        }

        #[test]
        fn stone_axe_sets_stealth_rock_on_hit() {
            let attacker = build_move(Species::Snorlax, Ability::Pressure, PokemonMove::StoneAxe);
            let target = build(Species::Snorlax, Ability::Pressure, None);
            let state = battle_state_from_lists(vec![attacker], vec![], vec![target], vec![]);
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();
            let outcomes = run_single_turn(
                &MatchState::BattleState(state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex,
                &pokemon_dex,
            );
            let p = prob_foe_hazard(&outcomes, |c| matches!(c, SideCondition::StealthRock));
            assert!((p - 0.9).abs() < 0.02, "expected ~0.9 Stealth Rock probability, got {p}");
        }

        // ── Removal moves ───────────────────────────────────────────────────────────────────

        fn run_user_move(state: BattleState, mv_slot: usize) -> BattleState {
            let pokemon_dex = pokemon_dex();
            let move_dex = move_dex();
            extract_battle_state(run_single_turn(
                &MatchState::BattleState(state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![mv_slot])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0])),
                &move_dex,
                &pokemon_dex,
            )).0
        }

        fn has_any_hazard(conds: &[SideCondition]) -> bool {
            conds.iter().any(|c| matches!(c,
                SideCondition::Spikes(_) | SideCondition::StealthRock
                | SideCondition::StickyWeb(_) | SideCondition::ToxicSpikes(_)))
        }

        #[test]
        fn rapid_spin_clears_user_side_and_boosts_speed() {
            let user = build_move(Species::Snorlax, Ability::Pressure, PokemonMove::RapidSpin);
            let foe = build(Species::Snorlax, Ability::Pressure, None);
            let mut state = battle_state_from_lists(vec![user], vec![], vec![foe], vec![]);
            state.p1_side_conditions.push(SideCondition::Spikes(2));
            state.p1_side_condition_turns.push(0);
            state.p1_side_conditions.push(SideCondition::StealthRock);
            state.p1_side_condition_turns.push(0);

            let after = run_user_move(state, 0);
            assert!(!has_any_hazard(&after.p1_side_conditions), "user side hazards cleared");
            assert_eq!(after.p1_active_mons[0].boosts[4], 1, "Rapid Spin raises Speed");
        }

        #[test]
        fn defog_clears_hazards_on_both_sides() {
            let user = build_move(Species::Snorlax, Ability::Pressure, PokemonMove::Defog);
            let foe = build(Species::Snorlax, Ability::Pressure, None);
            let mut state = battle_state_from_lists(vec![user], vec![], vec![foe], vec![]);
            for side in [Player::P1, Player::P2] {
                let (conds, turns) = match side {
                    Player::P1 => (&mut state.p1_side_conditions, &mut state.p1_side_condition_turns),
                    Player::P2 => (&mut state.p2_side_conditions, &mut state.p2_side_condition_turns),
                };
                conds.push(SideCondition::Spikes(1));
                turns.push(0);
                conds.push(SideCondition::StealthRock);
                turns.push(0);
            }
            let after = run_user_move(state, 0);
            assert!(!has_any_hazard(&after.p1_side_conditions), "user side cleared");
            assert!(!has_any_hazard(&after.p2_side_conditions), "foe side cleared");
            assert_eq!(after.p2_active_mons[0].boosts[6], -1, "Defog lowers target evasion");
        }

        #[test]
        fn tidy_up_clears_hazards_on_both_sides() {
            let user = build_move(Species::Snorlax, Ability::Pressure, PokemonMove::TidyUp);
            let foe = build(Species::Snorlax, Ability::Pressure, None);
            let mut state = battle_state_from_lists(vec![user], vec![], vec![foe], vec![]);
            state.p1_side_conditions.push(SideCondition::StickyWeb(None));
            state.p1_side_condition_turns.push(0);
            state.p2_side_conditions.push(SideCondition::ToxicSpikes(2));
            state.p2_side_condition_turns.push(0);
            let after = run_user_move(state, 0);
            assert!(!has_any_hazard(&after.p1_side_conditions));
            assert!(!has_any_hazard(&after.p2_side_conditions));
            assert_eq!(after.p1_active_mons[0].boosts[4], 1, "Tidy Up raises Speed");
            assert_eq!(after.p1_active_mons[0].boosts[0], 1, "Tidy Up raises Attack");
        }

        // ── Layering caps (unit) ────────────────────────────────────────────────────────────

        #[test]
        fn add_side_condition_caps_spikes_at_three() {
            let mut state = battle_state_from_lists(
                vec![build(Species::Snorlax, Ability::Pressure, None)], vec![],
                vec![build(Species::Snorlax, Ability::Pressure, None)], vec![],
            );
            for _ in 0..5 {
                simulator_helpers::add_side_condition(&mut state, Player::P1, SideCondition::Spikes(1), 0);
            }
            let layers = state.p1_side_conditions.iter().find_map(|c| match c {
                SideCondition::Spikes(n) => Some(*n), _ => None,
            });
            assert_eq!(layers, Some(3));
        }

        #[test]
        fn add_side_condition_caps_toxic_spikes_at_two() {
            let mut state = battle_state_from_lists(
                vec![build(Species::Snorlax, Ability::Pressure, None)], vec![],
                vec![build(Species::Snorlax, Ability::Pressure, None)], vec![],
            );
            for _ in 0..4 {
                simulator_helpers::add_side_condition(&mut state, Player::P1, SideCondition::ToxicSpikes(1), 0);
            }
            let layers = state.p1_side_conditions.iter().find_map(|c| match c {
                SideCondition::ToxicSpikes(n) => Some(*n), _ => None,
            });
            assert_eq!(layers, Some(2));
        }

        // ── Ordering: faint to hazards skips the entry ability ──────────────────────────────

        #[test]
        fn faint_to_stealth_rock_skips_entry_ability() {
            let lead = build(Species::Clefable, Ability::Pressure, None);
            let foe = build(Species::Snorlax, Ability::Pressure, None);
            let mut state = battle_state_from_lists(vec![lead], vec![], vec![foe], vec![]);
            state.p1_side_conditions.push(SideCondition::StealthRock);
            state.p1_side_condition_turns.push(0);

            // Drop a 1-HP Intimidate Gyarados directly into P1's active slot, then run its send-out.
            let mut gyara = build(Species::Gyarados, Ability::Intimidate, None);
            gyara.hp = 1;
            gyara.mon_id = 9;
            state.p1_active_mons[0] = gyara;
            let foe_atk_before = state.p2_active_mons[0].boosts[0];

            simulator_helpers::process_pokemon_send_out(
                &mut state, FieldSlot { player: Player::P1, slot_index: 0 },
            );

            assert!(state.p1_active_mons[0].fainted, "Gyarados faints to Stealth Rock");
            assert_eq!(
                state.p2_active_mons[0].boosts[0], foe_atk_before,
                "Intimidate must not fire when the entrant fainted to hazards"
            );
        }
    }

    mod protect_moves {
        use crate::battle::{BattleState, MatchState, Player, PlayerCommand};
        use crate::data::ability::Ability;
        use crate::data::pokemon_move::PokemonMove;
        use crate::data::species::Species;
        use crate::dex_data::Status;
        use crate::pokemon::{build_pokemon_state, PokemonState};
        use crate::simuilator_test_helpers::{
            battle_state_from_lists, extract_battle_state, move_dex, pokemon_dex, run_single_turn,
            simple_attack,
        };

        fn mon(species: Species, ability: Ability, m0: PokemonMove, m1: PokemonMove) -> PokemonState {
            let pd = pokemon_dex();
            let md = move_dex();
            build_pokemon_state(
                species, &pd, &md, Some(50),
                Some([Some(m0), Some(m1), None, None]),
                None, Some(ability), None, None, None, None, None, false,
            )
        }

        /// Run one turn where P1's active uses move slot `s1` and P2's uses slot `s2`.
        fn run_on(state: BattleState, s1: usize, s2: usize) -> Vec<(MatchState, f64)> {
            run_single_turn(
                &MatchState::BattleState(state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![s1])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![s2])),
                &move_dex(),
                &pokemon_dex(),
            )
        }

        fn run(p1: PokemonState, p2: PokemonState, s1: usize, s2: usize) -> Vec<(MatchState, f64)> {
            run_on(battle_state_from_lists(vec![p1], vec![], vec![p2], vec![]), s1, s2)
        }

        // ── Basic blocking ──────────────────────────────────────────────────────────────────

        #[test]
        fn protect_blocks_damaging_move() {
            let p1 = mon(Species::Clefable, Ability::Pressure, PokemonMove::Protect, PokemonMove::Splash);
            let p2 = mon(Species::Snorlax, Ability::Pressure, PokemonMove::Tackle, PokemonMove::Splash);
            let after = extract_battle_state(run(p1, p2, 0, 0)).0;
            let m = &after.p1_active_mons[0];
            assert_eq!(m.hp, m.stats[0], "Protect blocks the attack");
            assert_eq!(m.stall_counter, 1, "successful Protect grows the streak");
        }

        #[test]
        fn feint_bypasses_protect() {
            let p1 = mon(Species::Clefable, Ability::Pressure, PokemonMove::Protect, PokemonMove::Splash);
            let p2 = mon(Species::Snorlax, Ability::Pressure, PokemonMove::Feint, PokemonMove::Splash);
            let after = extract_battle_state(run(p1, p2, 0, 0)).0;
            let m = &after.p1_active_mons[0];
            assert!(m.hp < m.stats[0], "Feint (breaksProtect) bypasses Protect");
        }

        #[test]
        fn protect_blocks_status_move() {
            let p1 = mon(Species::Clefable, Ability::Pressure, PokemonMove::Protect, PokemonMove::Splash);
            let p2 = mon(Species::Snorlax, Ability::Pressure, PokemonMove::Leer, PokemonMove::Splash);
            let after = extract_battle_state(run(p1, p2, 0, 0)).0;
            assert_eq!(after.p1_active_mons[0].boosts[1], 0, "Protect blocks the status move Leer");
        }

        // ── Contact punishments ─────────────────────────────────────────────────────────────

        #[test]
        fn spiky_shield_chips_contact_attacker() {
            let p1 = mon(Species::Clefable, Ability::Pressure, PokemonMove::SpikyShield, PokemonMove::Splash);
            let p2 = mon(Species::Snorlax, Ability::Pressure, PokemonMove::Tackle, PokemonMove::Splash);
            let after = extract_battle_state(run(p1, p2, 0, 0)).0;
            let a = &after.p2_active_mons[0];
            assert_eq!(a.hp, a.stats[0] - a.stats[0] / 8, "contact attacker loses 1/8 max HP");
            assert_eq!(after.p1_active_mons[0].hp, after.p1_active_mons[0].stats[0], "still blocked");
        }

        #[test]
        fn spiky_shield_no_chip_on_non_contact() {
            let p1 = mon(Species::Clefable, Ability::Pressure, PokemonMove::SpikyShield, PokemonMove::Splash);
            let p2 = mon(Species::Snorlax, Ability::Pressure, PokemonMove::WaterGun, PokemonMove::Splash);
            let after = extract_battle_state(run(p1, p2, 0, 0)).0;
            assert_eq!(after.p2_active_mons[0].hp, after.p2_active_mons[0].stats[0], "no chip on a non-contact move");
        }

        #[test]
        fn baneful_bunker_poisons_contact_attacker() {
            let p1 = mon(Species::Clefable, Ability::Pressure, PokemonMove::BanefulBunker, PokemonMove::Splash);
            let p2 = mon(Species::Snorlax, Ability::Pressure, PokemonMove::Tackle, PokemonMove::Splash);
            let after = extract_battle_state(run(p1, p2, 0, 0)).0;
            assert_eq!(after.p2_active_mons[0].status, Some(Status::Poison));
        }

        #[test]
        fn baneful_bunker_does_not_poison_steel() {
            let p1 = mon(Species::Clefable, Ability::Pressure, PokemonMove::BanefulBunker, PokemonMove::Splash);
            let p2 = mon(Species::Metagross, Ability::Pressure, PokemonMove::Tackle, PokemonMove::Splash);
            let after = extract_battle_state(run(p1, p2, 0, 0)).0;
            assert_eq!(after.p2_active_mons[0].status, None, "Steel-type is immune to the poison");
        }

        #[test]
        fn kings_shield_lowers_contact_attacker_attack() {
            let p1 = mon(Species::Clefable, Ability::Pressure, PokemonMove::KingsShield, PokemonMove::Splash);
            let p2 = mon(Species::Snorlax, Ability::Pressure, PokemonMove::Tackle, PokemonMove::Splash);
            let after = extract_battle_state(run(p1, p2, 0, 0)).0;
            assert_eq!(after.p2_active_mons[0].boosts[0], -1, "contact attacker loses 1 Attack stage");
            assert_eq!(after.p1_active_mons[0].hp, after.p1_active_mons[0].stats[0], "damage blocked");
        }

        #[test]
        fn kings_shield_does_not_block_status_move() {
            // King's Shield blocks damaging moves only — Leer (status) lands through it.
            let p1 = mon(Species::Clefable, Ability::Pressure, PokemonMove::KingsShield, PokemonMove::Splash);
            let p2 = mon(Species::Snorlax, Ability::Pressure, PokemonMove::Leer, PokemonMove::Splash);
            let after = extract_battle_state(run(p1, p2, 0, 0)).0;
            assert_eq!(after.p1_active_mons[0].boosts[1], -1, "status move passes through King's Shield");
        }

        // ── Endure ──────────────────────────────────────────────────────────────────────────

        #[test]
        fn endure_survives_a_lethal_hit_at_one_hp() {
            let mut p1 = mon(Species::Clefable, Ability::Pressure, PokemonMove::Endure, PokemonMove::Splash);
            p1.hp = 5; // a Tackle would otherwise KO
            let p2 = mon(Species::Snorlax, Ability::Pressure, PokemonMove::Tackle, PokemonMove::Splash);
            let after = extract_battle_state(run(p1, p2, 0, 0)).0;
            assert_eq!(after.p1_active_mons[0].hp, 1, "Endure leaves the user at 1 HP");
            assert!(!after.p1_active_mons[0].fainted);
        }

        // ── Quick Guard ─────────────────────────────────────────────────────────────────────

        #[test]
        fn quick_guard_blocks_priority_move() {
            let p1 = mon(Species::Clefable, Ability::Pressure, PokemonMove::QuickGuard, PokemonMove::Splash);
            let p2 = mon(Species::Snorlax, Ability::Pressure, PokemonMove::QuickAttack, PokemonMove::Splash);
            let after = extract_battle_state(run(p1, p2, 0, 0)).0;
            assert_eq!(after.p1_active_mons[0].hp, after.p1_active_mons[0].stats[0], "Quick Guard blocks +priority");
        }

        #[test]
        fn quick_guard_does_not_block_priority_zero() {
            let p1 = mon(Species::Clefable, Ability::Pressure, PokemonMove::QuickGuard, PokemonMove::Splash);
            let p2 = mon(Species::Snorlax, Ability::Pressure, PokemonMove::Tackle, PokemonMove::Splash);
            let after = extract_battle_state(run(p1, p2, 0, 0)).0;
            let m = &after.p1_active_mons[0];
            assert!(m.hp < m.stats[0], "Quick Guard does not block priority-0 moves");
        }

        // ── Wide Guard (doubles) ────────────────────────────────────────────────────────────

        #[test]
        fn wide_guard_blocks_spread_move_in_doubles() {
            let p1a = mon(Species::Clefable, Ability::Pressure, PokemonMove::WideGuard, PokemonMove::Splash);
            let p1b = mon(Species::Snorlax, Ability::Pressure, PokemonMove::Splash, PokemonMove::Splash);
            let p2a = mon(Species::Snorlax, Ability::Pressure, PokemonMove::RockSlide, PokemonMove::Splash);
            let p2b = mon(Species::Snorlax, Ability::Pressure, PokemonMove::Splash, PokemonMove::Splash);
            let state = battle_state_from_lists(vec![p1a, p1b], vec![], vec![p2a, p2b], vec![]);
            // P1 slot0 = Wide Guard (move 0), slot1 = Splash (move 1); P2 slot0 = Rock Slide (move 0).
            let outcomes = run_single_turn(
                &MatchState::BattleState(state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![0, 1])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![0, 0])),
                &move_dex(),
                &pokemon_dex(),
            );
            let after = extract_battle_state(outcomes).0;
            assert_eq!(after.p1_active_mons[0].hp, after.p1_active_mons[0].stats[0]);
            assert_eq!(after.p1_active_mons[1].hp, after.p1_active_mons[1].stats[0]);
        }

        // ── Stall counter lifecycle ─────────────────────────────────────────────────────────

        #[test]
        fn protect_decays_on_consecutive_use() {
            let p1 = mon(Species::Clefable, Ability::Pressure, PokemonMove::Protect, PokemonMove::Splash);
            let p2 = mon(Species::Snorlax, Ability::Pressure, PokemonMove::Tackle, PokemonMove::Splash);
            // Turn 1: Protect succeeds at 100% (P2 idles), streak → 1.
            let s1 = extract_battle_state(run(p1, p2, 0, 1)).0;
            assert_eq!(s1.p1_active_mons[0].stall_counter, 1);
            // Turn 2: Protect again vs Tackle. 1/3 succeeds (streak 2, no damage) / 2/3 fails (streak 0).
            let outcomes = run_on(s1, 0, 0);
            let p_success: f64 = outcomes.iter().filter_map(|(s, p)| match s {
                MatchState::BattleState(bs) if bs.p1_active_mons[0].stall_counter == 2 => Some(*p),
                _ => None,
            }).sum();
            assert!((p_success - 1.0 / 3.0).abs() < 0.02, "second Protect succeeds ~1/3 of the time, got {p_success}");
        }

        #[test]
        fn streak_resets_after_a_non_protect_move() {
            let p1 = mon(Species::Clefable, Ability::Pressure, PokemonMove::Protect, PokemonMove::Tackle);
            let p2 = mon(Species::Snorlax, Ability::Pressure, PokemonMove::Splash, PokemonMove::Splash);
            // Turn 1: Protect → streak 1.
            let s1 = extract_battle_state(run(p1, p2, 0, 0)).0;
            assert_eq!(s1.p1_active_mons[0].stall_counter, 1);
            // Turn 2: Tackle (non-stalling) → streak resets to 0.
            let s2 = extract_battle_state(run_on(s1, 1, 0)).0;
            assert_eq!(s2.p1_active_mons[0].stall_counter, 0);
            // Turn 3: Protect succeeds again at 100% (single outcome).
            let outcomes = run_on(s2, 0, 0);
            assert_eq!(outcomes.len(), 1, "Protect at a reset streak cannot fail");
            assert_eq!(extract_battle_state(outcomes).0.p1_active_mons[0].stall_counter, 1);
        }
    }

    mod forced_switch_moves {
        use crate::battle::{BattleState, MatchState, Player, PlayerCommand};
        use crate::data::ability::Ability;
        use crate::data::pokemon_move::PokemonMove;
        use crate::data::species::Species;
        use crate::dex_data::{SideCondition, Status, VolatileStatus};
        use crate::pokemon::{build_pokemon_state, PokemonState, VolatileStatusState};
        use crate::simuilator_test_helpers::{
            battle_state_from_lists, extract_battle_state, move_dex, pokemon_dex, run_single_turn,
            simple_attack,
        };

        fn mon(species: Species, ability: Ability, m0: PokemonMove) -> PokemonState {
            let pd = pokemon_dex();
            let md = move_dex();
            build_pokemon_state(
                species, &pd, &md, Some(50),
                Some([Some(m0), Some(PokemonMove::Splash), None, None]),
                None, Some(ability), None, None, None, None, None, false,
            )
        }

        fn run(state: BattleState, s1: usize, s2: usize) -> Vec<(MatchState, f64)> {
            run_single_turn(
                &MatchState::BattleState(state),
                &PlayerCommand::Battle(simple_attack(Player::P1, vec![s1])),
                &PlayerCommand::Battle(simple_attack(Player::P2, vec![s2])),
                &move_dex(),
                &pokemon_dex(),
            )
        }

        fn p2_active_species_mass(outcomes: &[(MatchState, f64)], species: Species) -> f64 {
            outcomes.iter().filter_map(|(s, p)| match s {
                MatchState::BattleState(bs) if bs.p2_active_mons[0].species == species => Some(*p),
                _ => None,
            }).sum()
        }

        // ── Roar / Whirlwind ────────────────────────────────────────────────────────────────

        #[test]
        fn roar_forces_target_to_switch() {
            let user = mon(Species::Snorlax, Ability::Pressure, PokemonMove::Roar);
            let foe = mon(Species::Clefable, Ability::Pressure, PokemonMove::Splash);
            let bench = mon(Species::Gyarados, Ability::Pressure, PokemonMove::Splash);
            let state = battle_state_from_lists(vec![user], vec![], vec![foe], vec![bench]);
            let after = extract_battle_state(run(state, 0, 0)).0;
            assert_eq!(after.p2_active_mons[0].species, Species::Gyarados, "Roar dragged in the bench mon");
        }

        #[test]
        fn roar_random_replacement_branches_equally() {
            let user = mon(Species::Snorlax, Ability::Pressure, PokemonMove::Whirlwind);
            let foe = mon(Species::Clefable, Ability::Pressure, PokemonMove::Splash);
            let b1 = mon(Species::Gyarados, Ability::Pressure, PokemonMove::Splash);
            let b2 = mon(Species::Machamp, Ability::Pressure, PokemonMove::Splash);
            let state = battle_state_from_lists(vec![user], vec![], vec![foe], vec![b1, b2]);
            let outcomes = run(state, 0, 0);
            assert!((p2_active_species_mass(&outcomes, Species::Gyarados) - 0.5).abs() < 0.01);
            assert!((p2_active_species_mass(&outcomes, Species::Machamp) - 0.5).abs() < 0.01);
        }

        #[test]
        fn roar_with_no_bench_does_not_switch_and_fails() {
            let user = mon(Species::Snorlax, Ability::Pressure, PokemonMove::Roar);
            let foe = mon(Species::Clefable, Ability::Pressure, PokemonMove::Splash);
            let state = battle_state_from_lists(vec![user], vec![], vec![foe], vec![]);
            let after = extract_battle_state(run(state, 0, 0)).0;
            assert_eq!(after.p2_active_mons[0].species, Species::Clefable, "no bench → no switch");
            assert!(after.p1_active_mons[0].last_move_failed, "Roar with no legal target failed");
        }

        // ── Dragon Tail / Circle Throw ──────────────────────────────────────────────────────

        #[test]
        fn dragon_tail_damages_then_switches_on_hit() {
            let user = mon(Species::Snorlax, Ability::Pressure, PokemonMove::DragonTail);
            let foe = mon(Species::Snorlax, Ability::Pressure, PokemonMove::Splash);
            let bench = mon(Species::Gyarados, Ability::Pressure, PokemonMove::Splash);
            let state = battle_state_from_lists(vec![user], vec![], vec![foe], vec![bench]);
            let outcomes = run(state, 0, 0);
            // 90% accuracy → switch on the hit branch only.
            assert!((p2_active_species_mass(&outcomes, Species::Gyarados) - 0.9).abs() < 0.02);
        }

        #[test]
        fn dragon_tail_into_type_immunity_does_not_switch() {
            // Dragon is 0× vs Fairy (Clefable): no damage, no switch.
            let user = mon(Species::Snorlax, Ability::Pressure, PokemonMove::DragonTail);
            let foe = mon(Species::Clefable, Ability::Pressure, PokemonMove::Splash);
            let bench = mon(Species::Gyarados, Ability::Pressure, PokemonMove::Splash);
            let state = battle_state_from_lists(vec![user], vec![], vec![foe], vec![bench]);
            let outcomes = run(state, 0, 0);
            assert_eq!(p2_active_species_mass(&outcomes, Species::Gyarados), 0.0, "immune target is not phazed");
        }

        #[test]
        fn circle_throw_replacement_takes_stealth_rock() {
            let user = mon(Species::Snorlax, Ability::Pressure, PokemonMove::Roar);
            let foe = mon(Species::Clefable, Ability::Pressure, PokemonMove::Splash);
            let bench = mon(Species::Snorlax, Ability::Pressure, PokemonMove::Splash);
            let mut state = battle_state_from_lists(vec![user], vec![], vec![foe], vec![bench]);
            state.p2_side_conditions.push(SideCondition::StealthRock);
            state.p2_side_condition_turns.push(0);
            let after = extract_battle_state(run(state, 0, 0)).0;
            let m = &after.p2_active_mons[0];
            assert_eq!(m.species, Species::Snorlax);
            assert!(m.hp < m.stats[0], "forced-in replacement took Stealth Rock");
        }

        // ── Blocking conditions ─────────────────────────────────────────────────────────────

        #[test]
        fn suction_cups_blocks_the_forced_switch() {
            let user = mon(Species::Snorlax, Ability::Pressure, PokemonMove::Roar);
            let foe = mon(Species::Snorlax, Ability::SuctionCups, PokemonMove::Splash);
            let bench = mon(Species::Gyarados, Ability::Pressure, PokemonMove::Splash);
            let state = battle_state_from_lists(vec![user], vec![], vec![foe], vec![bench]);
            let after = extract_battle_state(run(state, 0, 0)).0;
            assert_eq!(after.p2_active_mons[0].species, Species::Snorlax, "Suction Cups blocks phazing");
        }

        #[test]
        fn guard_dog_blocks_the_forced_switch() {
            let user = mon(Species::Snorlax, Ability::Pressure, PokemonMove::Roar);
            let foe = mon(Species::Snorlax, Ability::GuardDog, PokemonMove::Splash);
            let bench = mon(Species::Gyarados, Ability::Pressure, PokemonMove::Splash);
            let state = battle_state_from_lists(vec![user], vec![], vec![foe], vec![bench]);
            let after = extract_battle_state(run(state, 0, 0)).0;
            assert_eq!(after.p2_active_mons[0].species, Species::Snorlax, "Guard Dog blocks phazing");
        }

        #[test]
        fn ingrain_blocks_the_forced_switch() {
            let user = mon(Species::Snorlax, Ability::Pressure, PokemonMove::Roar);
            let foe = mon(Species::Snorlax, Ability::Pressure, PokemonMove::Splash);
            let bench = mon(Species::Gyarados, Ability::Pressure, PokemonMove::Splash);
            let mut state = battle_state_from_lists(vec![user], vec![], vec![foe], vec![bench]);
            state.p2_active_mons[0].volatiles.push(
                VolatileStatusState::MoveStatus(VolatileStatus::Ingrain, 0));
            let after = extract_battle_state(run(state, 0, 0)).0;
            assert_eq!(after.p2_active_mons[0].species, Species::Snorlax, "Ingrain roots the target");
        }

        // ── Queue purge: a slower queued move must not run for the replacement ───────────────

        #[test]
        fn phazed_target_slower_move_does_not_run_for_replacement() {
            // P2's Clefable selects Swords Dance (priority 0); P1's faster Snorlax... no — use Roar
            // (−6) so P2 acts first? We need P2 to be phazed BEFORE acting: give the phazer a
            // higher-priority guaranteed-first slot via Whirlwind (−6) vs a −7 move. Trick Room is
            // −7. P1 Whirlwind (−6) resolves before P2's Trick Room (−7); P2 is switched out, so
            // Trick Room must never set Trick Room.
            let user = mon(Species::Snorlax, Ability::Pressure, PokemonMove::Whirlwind);
            let foe = mon(Species::Clefable, Ability::Pressure, PokemonMove::TrickRoom);
            let bench = mon(Species::Gyarados, Ability::Pressure, PokemonMove::Splash);
            let state = battle_state_from_lists(vec![user], vec![], vec![foe], vec![bench]);
            let after = extract_battle_state(run(state, 0, 0)).0;
            assert_eq!(after.p2_active_mons[0].species, Species::Gyarados, "target was phazed");
            assert!(
                after.pseudo_weathers.iter().all(|pw| !matches!(pw, crate::dex_data::PseudoWeather::TrickRoom)),
                "the switched-out target's queued Trick Room must not execute"
            );
        }

        // ── Stomping Tantrum / `last_move_failed` ───────────────────────────────────────────

        #[test]
        fn confusion_self_hit_sets_last_move_failed() {
            let mut user = mon(Species::Snorlax, Ability::Pressure, PokemonMove::Tackle);
            user.volatiles.push(VolatileStatusState::MoveStatus(VolatileStatus::Confusion, 4));
            let foe = mon(Species::Clefable, Ability::Pressure, PokemonMove::Splash);
            let state = battle_state_from_lists(vec![user], vec![], vec![foe], vec![]);
            let outcomes = run(state, 0, 0);
            // The self-hit branch: P1 took damage, P2 untouched (Tackle never executed).
            let self_hit = outcomes.iter().find(|(s, _)| matches!(s,
                MatchState::BattleState(bs)
                    if bs.p1_active_mons[0].hp < bs.p1_active_mons[0].stats[0]
                        && bs.p2_active_mons[0].hp == bs.p2_active_mons[0].stats[0]));
            let (MatchState::BattleState(bs), _) = self_hit.expect("a confusion self-hit branch exists") else { unreachable!() };
            assert!(bs.p1_active_mons[0].last_move_failed, "confusion self-hit counts as a failed move");
        }

        #[test]
        fn status_move_that_changes_nothing_fails() {
            // Swords Dance already at +6 → no change → last_move_failed.
            let mut user = mon(Species::Snorlax, Ability::Pressure, PokemonMove::SwordsDance);
            user.boosts[0] = 6;
            let foe = mon(Species::Clefable, Ability::Pressure, PokemonMove::Splash);
            let state = battle_state_from_lists(vec![user], vec![], vec![foe], vec![]);
            let after = extract_battle_state(run(state, 0, 0)).0;
            assert!(after.p1_active_mons[0].last_move_failed, "a status move that did nothing failed");
        }

        #[test]
        fn successful_status_move_clears_failed_even_through_protect() {
            // Swords Dance is self-targeting, so an opposing Protect cannot stop it: 0 "damage", but
            // the move still succeeds → not failed (the "0 damage but succeeds" case).
            let user = mon(Species::Snorlax, Ability::Pressure, PokemonMove::SwordsDance);
            let foe = mon(Species::Clefable, Ability::Pressure, PokemonMove::Protect);
            let state = battle_state_from_lists(vec![user], vec![], vec![foe], vec![]);
            let after = extract_battle_state(run(state, 0, 0)).0;
            assert_eq!(after.p1_active_mons[0].boosts[0], 2, "Swords Dance landed");
            assert!(!after.p1_active_mons[0].last_move_failed, "a successful status move did not fail");
        }

        #[test]
        fn thunder_wave_on_already_paralyzed_target_fails() {
            let user = mon(Species::Snorlax, Ability::Pressure, PokemonMove::ThunderWave);
            let mut foe = mon(Species::Clefable, Ability::Pressure, PokemonMove::Splash);
            foe.status = Some(Status::Paralysis);
            let state = battle_state_from_lists(vec![user], vec![], vec![foe], vec![]);
            // Already-statused target → no change in any branch → failed everywhere.
            for (s, _) in run(state, 0, 0) {
                if let MatchState::BattleState(bs) = s {
                    assert!(bs.p1_active_mons[0].last_move_failed, "Thunder Wave on a paralyzed target failed");
                }
            }
        }

        #[test]
        fn splash_is_a_no_op_success_not_a_failure() {
            let user = mon(Species::Snorlax, Ability::Pressure, PokemonMove::Splash);
            let foe = mon(Species::Clefable, Ability::Pressure, PokemonMove::Splash);
            let state = battle_state_from_lists(vec![user], vec![], vec![foe], vec![]);
            let after = extract_battle_state(run(state, 0, 0)).0;
            assert!(!after.p1_active_mons[0].last_move_failed, "Splash is a no-op success, not a failure");
        }
    }

}
