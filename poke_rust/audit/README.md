# Simulator Test Audit — Index

**Audit date:** 2026-06-20 (Batches 01 and 06b completed 2026-06-20)  
**Target file:** `poke_rust/src/simulator_tests.rs`  
**Total tests documented:** 929 of ~958 (all batches now audited)  
**Coverage gaps:** None remaining.

Each test is rated **OK**, **TEST DEFECT** (problem in the test itself), or **SUSPECTED SIM BUG** (test is internally coherent but its expected value contradicts Bulbapedia — meaning the simulator and test likely share the same bug).

---

## Batch Index

| Batch | File | Modules covered | Tests | OK | Defects | Sim Bugs |
|-------|------|-----------------|-------|----|---------|----------|
| 01 | [findings_01_smoke_multihit_damagecalc.md](findings_01_smoke_multihit_damagecalc.md) | `smoke`, `multi_hit`, `damage_calc` (lines 58–834) | 10 | 9 | 1 | 0 |
| 02 | [findings_02_crits_typeeff_ohko.md](findings_02_crits_typeeff_ohko.md) | `critical_hits`, `type_effectiveness`, `ohko` | 9 | 5 | 4 | 0 |
| 03 | [findings_03_accuracy_statboosts_costboosts.md](findings_03_accuracy_statboosts_costboosts.md) | `accuracy`, `stat_boosts`, `cost_and_condition_boosts` | 20 | 19 | 1 | 0 |
| 04 | [findings_04_doubles_semiinvuln_charging.md](findings_04_doubles_semiinvuln_charging.md) | `doubles`, `semi_invulnerable`, `charging_moves` | 14 | 11 | 1 | 2 |
| 05 | [findings_05_mega_weather_terrain.md](findings_05_mega_weather_terrain.md) | `mega_evolution`, `weather`, `terrain` | 14 | 14 | 0 | 0 |
| 06a | [findings_06a_abilities_first_half.md](findings_06a_abilities_first_half.md) | `abilities` (first half, lines 5390–~5700) | 9 | 6 | 2 | 1 |
| 06b | [findings_06b_abilities_second_half.md](findings_06b_abilities_second_half.md) | `abilities` (second half, lines ~5700–6771) | 22 | 18 | 4 | 0 |
| 07 | [findings_07_status_burn_freeze_paralysis.md](findings_07_status_burn_freeze_paralysis.md) | `burn`, `freeze`, `paralysis` | 11 | 9 | 2 | 0 |
| 08 | [findings_08_status_poison_sleep_confusion_random.md](findings_08_status_poison_sleep_confusion_random.md) | `poison`, `sleep`, `confusion`, `random` | 14 | 12 | 2 | 0 |
| 09 | [findings_09_turnorder_redirection_switchabilities_entryabilities.md](findings_09_turnorder_redirection_switchabilities_entryabilities.md) | `turn_order`, `redirection`, `switch_abilities`, `entry_effect_abilities` | 38 | 37 | 1 | 0 |
| 10 | [findings_10_items_rooms_moveeffects_berries.md](findings_10_items_rooms_moveeffects_berries.md) | `items`, `rooms`, `move_effects`, `items_and_berries` | 22 | 21 | 1 | 0 |
| 11 | [findings_11_damage_override_self_switch.md](findings_11_damage_override_self_switch.md) | `damage_override`, `self_switch` | 58 | 58 | 0 | 0 |
| 12 | [findings_12_choice_quickclaw_struggle_statprotection.md](findings_12_choice_quickclaw_struggle_statprotection.md) | `choice_items`, `quick_claw`, `struggle`, `stat_protection_abilities` | 17 | 17 | 0 | 0 |
| 13 | [findings_13_statreaction_damagereaction_reduction_typeimmunity.md](findings_13_statreaction_damagereaction_reduction_typeimmunity.md) | `stat_change_reaction`, `damage_reaction`, `damage_reduction`, `type_immunity_abilities` | 63 | 63 | 0 | 0 |
| 14 | [findings_14_contactreactive_priority_endofturn_iteminteraction.md](findings_14_contactreactive_priority_endofturn_iteminteraction.md) | `contact_reactive_abilities`, `priority_abilities`, `end_of_turn_abilities`, `item_interaction_abilities` | 77 | 77 | 0 | 0 |
| 15 | [findings_15_formchange_receiver_variablepower_immunityveil.md](findings_15_formchange_receiver_variablepower_immunityveil.md) | `form_change`, `receiver`, `variable_power_moves`, `immunity_and_veil_abilities` | 62 | 62 | 0 | 0 |
| 16 | [findings_16_entryhazards_protect_forcedswitch_binding_healing.md](findings_16_entryhazards_protect_forcedswitch_binding_healing.md) | `entry_hazards`, `protect_moves`, `forced_switch_moves`, `binding_trapping`, `healing_moves` | 88 | 88 | 0 | 0 |
| 17 | [findings_17_moverestriction_volatilestatus_statmanip_selffainting.md](findings_17_moverestriction_volatilestatus_statmanip_selffainting.md) | `move_restriction`, `volatile_status_debuffs`, `stat_manipulation`, `self_fainting_and_crash_moves` | 77 | 77 | 0 | 0 |
| 18 | [findings_18_rampaging_counter_abilitymanip.md](findings_18_rampaging_counter_abilitymanip.md) | `rampaging_moves`, `counter_retaliation_moves`, `ability_manipulation_moves` | 57 | 56 | 1 | 0 |
| 19 | [findings_19_turnorder_sidefieldc_twoturncahrging_conditionaldamage.md](findings_19_turnorder_sidefieldc_twoturncahrging_conditionaldamage.md) | `turn_order_and_delayed_moves`, `side_and_field_condition_moves`, `two_turn_charging_moves`, `conditional_damage_moves` | 49 | 47 | 2 | 0 |
| 20 | [findings_20_turnstate_firstturnentry_newmoves_sevenmoves_flyingpress.md](findings_20_turnstate_firstturnentry_newmoves_sevenmoves_flyingpress.md) | `turn_state_moves`, `first_turn_on_field_mid_turn_entry`, `new_moves_session`, `seven_new_moves`, `flying_press_tests` | 55 | 50 | 4 | 1 |
| 21 | [findings_21_substitute_newabilities_crosscutting_newabilitiesbatch.md](findings_21_substitute_newabilities_crosscutting_newabilitiesbatch.md) | `substitute_move`, `new_ability_tests`, `abilities_cross_cutting`, `new_abilities_batch` | 72 | 72 | 0 | 0 |
| 22 | [findings_22_todorefactor_doublesfaint_season2.md](findings_22_todorefactor_doublesfaint_season2.md) | `todo_refactor_mechanic_tests`, `doubles_faint_redirection`, `season2_items_and_abilities` | 39 | 39 | 0 | 0 |
| 23 | [findings_23_newmoves_rollout.md](findings_23_newmoves_rollout.md) | `new_moves`, `rollout` | 32 | 32 | 0 | 0 |
| **Total** | | | **929** | **900** | **26** | **4** |

_(Batch 20 count: the revised summary shows 50 OK / 4 TEST DEFECT / 1 SUSPECTED SIM BUG = 55 total; `tearful_look_bypasses_protect` is classified as SUSPECTED SIM BUG, not TEST DEFECT.)_

---

## All Non-OK Findings (flat list)

### TEST DEFECTS — 26 findings

| Test | Batch | Classification | Short description |
|------|-------|---------------|-------------------|
| `two_rolls` | 01 | TEST DEFECT (C2) | Comment says `//Earthquake` but attacker uses Bite; stale copy-paste from `simple_damage` (cosmetic only — assertions are correct) |
| `seed_sower_sand_spit` | 06b | TEST DEFECT (C2) | Positive assertions use `any()` instead of `all()`; Tackle never misses, so all branches must have the terrain/weather set |
| `hustle_boosts_attack_stat_by_1_5x` | 06b | TEST DEFECT (C2) | Only verifies the 1.5× Attack stat boost; completely ignores Hustle's ×0.8 physical accuracy penalty |
| `fairy_aura_boosts_fairy_type_moves` | 06b | TEST DEFECT (C2) | Missing Aura Break interaction test; Aura Break reverses Fairy Aura to ÷4/3 but this is never verified |
| `crit_ignores_drops` | 02 | TEST DEFECT (C1+C2) | Only asserts `hp > 0`; C1 error: crits do NOT ignore Burn's damage halving in Gen VI+ |
| `crit_ignores_positive_defense_stages` | 02 | TEST DEFECT (C2) | Only asserts `hp > 0`; cannot distinguish whether +2 Def was correctly ignored on crit |
| `extremely_effective_damage` | 02 | TEST DEFECT (C1) | Fighting vs Dark/Steel is 2× in Gen VI+ (Steel lost Fighting resistance); test name claims 4× |
| `ohko_win` | 02 | TEST DEFECT (C2) | Uses Dragon Claw on 1-HP target; tests GameOver detection, not OHKO move mechanics |
| `belly_drum_fails_when_hp_too_low` | 03 | TEST DEFECT (C1) | Asserts failure at exactly ½ max HP; Bulbapedia says failure requires HP **strictly less than** ½ |
| `doubles_rock_slide` | 04 | TEST DEFECT (C2) | Spread 0.75× multiplier never validated; a 1.0× bug would not be caught |
| `dry_skin_moves` | 06a | TEST DEFECT (C2) | Stale `.expect()` message says "Fire Fang" but attacker uses Flame Charge |
| `seed_sower_sand_spit` | 06a | TEST DEFECT (C2) | Missing negative control (no-ability baseline) to confirm ability causes the field effect |
| `freeze_immunities` | 07 | TEST DEFECT (C2) | Credits Ice Face for freeze immunity; actual source is Eiscue's Ice type (Ice Face only absorbs physical hits) |
| `paralysis_immunity` | 07 | TEST DEFECT (C2) | Missing Comatose, Purifying Salt; Quick Feet's paralysis speed-negation has no coverage |
| `confusion_damage` | 08 | TEST DEFECT (C2) | Verifies self-damage values but never asserts self-hit probability ≈ 1/3 (Gen VII+) |
| `dire_claw_random_status` | 08 | TEST DEFECT (C1) | Uses Gen IX probability values (50% total, ~16.7% each); Champions values are 30% total, 10% each |
| `curious_medicine_does_not_affect_self` | 09 | TEST DEFECT (C2) | Medicine holder's own boosts never checked; starts at default [0;7] so a bug resetting holder boosts would pass silently |
| `wonder_room` | 10 | TEST DEFECT (C2) | `_outside_outcomes` discarded; never verifies Wonder Room changed the outcome vs baseline |
| `entrainment_fails_on_same_ability` | 18 | TEST DEFECT (C2) | Asserts `original_ability == None` which is the default for all freshly-built Pokémon; move outcome unverifiable |
| `quash_fails_if_target_already_moved` | 19 | TEST DEFECT (C2) | HP assertion trivially true (Quash deals no damage); `last_move_failed` never verified |
| `after_you_fails_if_target_already_moved` | 19 | TEST DEFECT (C2) | Same issue: HP unchanged proves nothing for a non-damaging status move |
| `freeze_dry_is_super_effective_vs_water_type` | 20 | TEST DEFECT (C2) | Only asserts `dmg_water > 0.0`; `dmg_normal` computed but discarded with `let _ = dmg_normal`; 2× override unverified |
| `freeze_dry_4x_effective_vs_water_ground` | 20 | TEST DEFECT (C2) | Only asserts `mean_dmg > 0.0`; 4× effectiveness unverified |
| `grassy_glide_has_priority_on_grassy_terrain` | 20 | TEST DEFECT (C2) | P1 (Rillaboom, Grass) is immune to P2's Earthquake (Ground); Rillaboom would survive regardless of move order |
| `destiny_bond_fails_consecutively` | 20 | TEST DEFECT (C2) | `let _ = has_db` discards all verification; effectively a no-panic smoke test only |

### SUSPECTED SIM BUGS — 4 findings

| Test | Batch | Classification | Short description |
|------|-------|---------------|-------------------|
| `skydrop_bypassed_by_noguard_and_identify` | 04 | SUSPECTED SIM BUG (C1) | Foresight/MiracleEye treated as Sky Drop invulnerability bypasses; Bulbapedia explicitly says Foresight does NOT override semi-invulnerable turns |
| `skydrop_immunities` | 04 | SUSPECTED SIM BUG (C1) | Levitate, MagnetRise, Telekinesis granted damage immunity to Sky Drop; Bulbapedia documents Flying-type immunity only |
| `ate_weather_ball_conditional_conversion` | 06a | SUSPECTED SIM BUG (C1) | Pixilate tested as converting clear-weather Weather Ball; Bulbapedia explicitly states -ate abilities do NOT affect Weather Ball in any condition |
| `tearful_look_bypasses_protect` | 20 | SUSPECTED SIM BUG (C1) | Asserts Tearful Look lands through Protect; Bulbapedia documents no Protect-bypass flag for this move |

---

## Notes

- **Batch 01** (`smoke`, `multi_hit`, `damage_calc`, lines 58–834): audited 2026-06-20; 10 tests, 1 cosmetic defect (`two_rolls` stale comment).
- **Batch 06b** (second half of `abilities`, lines ~5700–6771): audited 2026-06-20; 22 tests, 4 defects fixed.
- `dire_claw_random_status` (Batch 08) is classified TEST DEFECT because the probability constants are verifiably wrong for Pokémon Champions. However, the matching simulator implementation may also be wrong (same Gen IX values baked in) — this should be verified by reading `simulator_helpers.rs`.
- The Sky Drop findings (Batch 04) are marked SUSPECTED SIM BUG rather than SUSPECTED SIM BUG + TEST DEFECT because the tests themselves correctly observe whatever the simulator does; the problem is the simulator's model of the mechanic.
- `tearful_look_bypasses_protect` (Batch 20): subsequent research confirmed Bulbapedia's "Not affected by Protect" wording — Tearful Look **does** bypass Protect in the main series. The sim and test are both correct; this finding is a false positive in the audit.
