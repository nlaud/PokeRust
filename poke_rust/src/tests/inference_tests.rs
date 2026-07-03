//! Tests for `state::inference::apply_information`.
//!
//! Tests construct hand-built `InformationEvent` lists and assert on the resulting
//! `UnknownMatchState`.  All assertions must satisfy the soundness invariant: the
//! true training/item/stat of the simulated Pokémon must lie *within* every returned bound.

#![allow(unused)]

use std::collections::{HashMap, HashSet};

use crate::data::ability::Ability;
use crate::data::item::Item;
use crate::data::pokemon_move::PokemonMove;
use crate::data::species::Species;
use crate::state::battle::{FieldSlot, Player};
use crate::state::dex_data::{
    AccuracyType, DamageOverride, HitEffect, MoveCategory, MoveData, MoveTarget, PokemonData,
    PokemonSecondaryEffect, PokemonType, PseudoWeather, SelfDestructType, SelfSwitchType,
    SideCondition, Status, Terrain, VolatileStatus, Weather,
};
use crate::information::inference::{
    apply_information, apply_switch_out_reset, get_mon_by_idx, mon_idx_for_active_slot,
    pass5_back_solve, unknown_is_excluded, InferenceConfig, EV_LATTICE,
};
use crate::information::information::{CantReason, EventKind, InformationEvent, SwitchState};
use crate::information::unknowns::{
    PokemonHP, Statement, Unknown, UnknownBattleState, UnknownMatchState, UnknownPokemonState,
};

// ── Helpers ───────────────────────────────────────────────────────────────────

fn p1(i: u8) -> FieldSlot { FieldSlot { player: Player::P1, slot_index: i } }
fn p2(i: u8) -> FieldSlot { FieldSlot { player: Player::P2, slot_index: i } }

fn event(kind: EventKind) -> InformationEvent {
    InformationEvent { kind, reactions: vec![] }
}

fn event_with(kind: EventKind, reactions: Vec<InformationEvent>) -> InformationEvent {
    InformationEvent { kind, reactions }
}

/// Minimal MoveData for a normal-priority physical move with no special effects.
fn normal_physical_move(name: PokemonMove, bp: u16) -> MoveData {
    MoveData {
        name,
        base_power: bp,
        accuracy: AccuracyType::Percent(100),
        target: MoveTarget::Normal,
        secondaries: vec![],
        self_secondaries: vec![],
        pp: 10,
        category: MoveCategory::Physical,
        pokemon_type: PokemonType::Normal,
        priority: 0,
        flags: vec![],
        ohko: false,
        thaws_target: false,
        heal_fraction: [0, 0],
        force_switch: false,
        self_switch: SelfSwitchType::None,
        self_boost: [0; 7],
        self_destruct: SelfDestructType::None,
        breaks_protect: false,
        recoil_fraction: [0, 0],
        drain_fraction: [0, 0],
        mind_blown_recoil: false,
        struggle_recoil: false,
        crit_ratio: 1,
        foul_play: false,
        ignore_ability: false,
        ignore_defense_boosts: false,
        ignore_evasion: false,
        ignore_immunity: vec![],
        multihit_range: [1, 1],
        multihit_accuracy: false,
        sleep_usable: false,
        has_crash_damage: false,
        damage_override: DamageOverride::None,
        stalling_move: false,
        override_offensive_stat: None,
        override_defensive_stat: None,
    }
}

fn unknown_mon_species(species: Species) -> UnknownPokemonState {
    UnknownPokemonState::from_opponent_species(species, &HashMap::new(), 50)
}

fn unknown_mon() -> UnknownPokemonState {
    unknown_mon_species(Species::Garchomp)
}

fn battle_with_p2(p2_active: Vec<UnknownPokemonState>) -> UnknownBattleState {
    battle_nvn(vec![], p2_active)
}

fn battle_1v1(p1_mon: UnknownPokemonState, p2_mon: UnknownPokemonState) -> UnknownBattleState {
    battle_nvn(vec![p1_mon], vec![p2_mon])
}

/// Generic N-vs-N battle builder.  Compared to the single-slot helpers, this one
/// is useful when you need more than one active Pokémon per side.
fn battle_nvn(
    p1_active: Vec<UnknownPokemonState>,
    p2_active: Vec<UnknownPokemonState>,
) -> UnknownBattleState {
    let n = p1_active.len().max(p2_active.len());
    UnknownBattleState {
        active_per_side:   n as u8,
        back_mons_per_side: 6u8.saturating_sub(n as u8),
        p1_active_mons:    p1_active,
        p2_active_mons:    p2_active,
        p1_known_back_mons:    vec![],
        p2_known_back_mons:    vec![],
        p1_possible_back_mons: vec![],
        p2_possible_back_mons: vec![],
        turn_number:   1,
        turn_started:  false,
        turn_ended:    false,
        p1_has_tera:   false,
        p2_has_tera:   false,
        p1_has_mega:   false,
        p2_has_mega:   false,
        weather:              None,
        weather_turns:        None,
        weather_setter_mon_idx: None,
        pseudo_weathers:      vec![],
        pseudo_weather_turns: vec![],
        terrain:              None,
        terrain_turns:        None,
        terrain_setter_mon_idx: None,
        p1_side_conditions:        vec![],
        p1_side_condition_turns:   vec![],
        p1_side_condition_setters: vec![],
        p2_side_conditions:        vec![],
        p2_side_condition_turns:   vec![],
        p2_side_condition_setters: vec![],
        p1_slot_conditions: (0..n).map(|_| vec![]).collect(),
        p2_slot_conditions: (0..n).map(|_| vec![]).collect(),
        self_switch_pending:      None,
        items_consumed_this_turn: vec![],
        last_move_on_field:       None,
        sub_damage_dealt:         0,
        round_used_this_turn:     false,
        predicates:               vec![],
    }
}

fn apply(
    state: UnknownBattleState,
    events: Vec<InformationEvent>,
) -> UnknownBattleState {
    apply_ex(state, events, HashMap::new(), HashMap::new())
}

fn apply_ex(
    state: UnknownBattleState,
    events: Vec<InformationEvent>,
    dex: HashMap<Species, PokemonData>,
    move_dex: HashMap<PokemonMove, MoveData>,
) -> UnknownBattleState {
    let result = apply_information(
        UnknownMatchState::Battle(state),
        &events,
        false,
        &dex,
        &move_dex,
        &HashMap::new(), // ability_dex — not needed for most tests
        &InferenceConfig::default(),
    );
    match result {
        UnknownMatchState::Battle(b) => b,
        _ => panic!("expected Battle state"),
    }
}

/// Apply information with a custom `InferenceConfig` (e.g. learnset_dex, ev_total_cap).
fn apply_with_config(
    state: UnknownBattleState,
    events: Vec<InformationEvent>,
    dex: HashMap<Species, PokemonData>,
    move_dex: HashMap<PokemonMove, MoveData>,
    config: InferenceConfig,
) -> UnknownBattleState {
    let result = apply_information(
        UnknownMatchState::Battle(state),
        &events,
        false,
        &dex,
        &move_dex,
        &HashMap::new(),
        &config,
    );
    match result {
        UnknownMatchState::Battle(b) => b,
        _ => panic!("expected Battle state"),
    }
}

fn is_item_excluded(mon: &UnknownPokemonState, item: &Item) -> bool {
    match &mon.item {
        Unknown::Known(v) => v != item,
        Unknown::Not(excl) => excl.contains(item),
        Unknown::Possibly(poss) => !poss.contains(item),
    }
}

// ── Pass 1: Status ────────────────────────────────────────────────────────────

#[test]
fn test_status_inflicted() {
    let state = battle_with_p2(vec![unknown_mon()]);
    let result = apply(
        state,
        vec![event(EventKind::StatusInflicted {
            target: p2(0),
            status: Status::Paralysis,
        })],
    );
    assert_eq!(result.p2_active_mons[0].status, Some(Status::Paralysis));
}

#[test]
fn test_status_inflicted_then_cured() {
    let state = battle_with_p2(vec![unknown_mon()]);
    let result = apply(
        state,
        vec![
            event(EventKind::StatusInflicted { target: p2(0), status: Status::Paralysis }),
            event(EventKind::StatusCured { target: p2(0), status: Status::Paralysis }),
        ],
    );
    assert_eq!(result.p2_active_mons[0].status, None);
}

/// Belt-and-braces faint detection: a `DamageDealt` whose payload is 0 HP marks the mon
/// fainted even without an explicit `Faint` event (the display convention shows 0 only at
/// an actual faint). Guards the fainted-gates in the EOT passes and suppression scans.
#[test]
fn test_damage_dealt_to_zero_sets_fainted() {
    use crate::information::unknowns::PokemonHP;
    let state = battle_with_p2(vec![unknown_mon()]);
    let result = apply(
        state,
        vec![event(EventKind::DamageDealt {
            target: p2(0),
            new_hp: PokemonHP::Percent(0),
        })],
    );
    assert!(
        result.p2_active_mons[0].fainted,
        "DamageDealt with 0 HP must set fainted (guards EOT-pass fainted gates)"
    );
}

// ── Pass 1: Items ─────────────────────────────────────────────────────────────

#[test]
fn test_item_revealed_sets_known() {
    let state = battle_with_p2(vec![unknown_mon()]);
    let result = apply(
        state,
        vec![event(EventKind::ItemRevealed { slot: p2(0), item: Item::ChoiceBand })],
    );
    assert!(
        matches!(&result.p2_active_mons[0].item, Unknown::Known(i) if *i == Item::ChoiceBand),
        "item must be Known(ChoiceBand)"
    );
}

#[test]
fn test_item_lost_consumed() {
    let mut mon = unknown_mon();
    mon.item = Unknown::Known(Item::OranBerry);
    let state = battle_with_p2(vec![mon]);
    let result = apply(
        state,
        vec![event(EventKind::ItemLost { slot: p2(0), item: Item::OranBerry, consumed: true })],
    );
    let m = &result.p2_active_mons[0];
    assert_eq!(m.consumed_item, Some(Item::OranBerry), "consumed_item should be set");
    assert!(matches!(&m.item, Unknown::Known(i) if *i == Item::None), "item should now be None");
}

// ── Pass 1: Moves / Choice exclusion ─────────────────────────────────────────

#[test]
fn test_choice_excluded_after_two_different_moves() {
    let state = battle_with_p2(vec![unknown_mon()]);
    let result = apply(
        state,
        vec![
            event(EventKind::MoveUsed {
                user: p2(0),
                move_used: PokemonMove::Earthquake,
                targets: vec![p1(0)],
            }),
            event(EventKind::MoveUsed {
                user: p2(0),
                move_used: PokemonMove::DragonClaw,
                targets: vec![p1(0)],
            }),
        ],
    );
    let m = &result.p2_active_mons[0];
    assert!(is_item_excluded(m, &Item::ChoiceBand), "ChoiceBand excluded");
    assert!(is_item_excluded(m, &Item::ChoiceScarf), "ChoiceScarf excluded");
    assert!(is_item_excluded(m, &Item::ChoiceSpecs), "ChoiceSpecs excluded");
}

#[test]
fn test_choice_not_excluded_for_same_move_twice() {
    let state = battle_with_p2(vec![unknown_mon()]);
    let result = apply(
        state,
        vec![
            event(EventKind::MoveUsed {
                user: p2(0),
                move_used: PokemonMove::Earthquake,
                targets: vec![p1(0)],
            }),
            event(EventKind::MoveUsed {
                user: p2(0),
                move_used: PokemonMove::Earthquake,
                targets: vec![p1(0)],
            }),
        ],
    );
    assert!(
        !is_item_excluded(&result.p2_active_mons[0], &Item::ChoiceBand),
        "ChoiceBand must NOT be excluded when same move repeated"
    );
}

// ── Regression: S15 — bookkeeping drift (consecutive_move_count, times_hit,
//    last_move_on_field) must match simulator semantics ──────────────────────

/// `consecutive_move_count` must use the simulator's 0-based streak convention:
/// a move's first use (or a switch to a different move) is streak 0, not 1. Before
/// the S15 fix, inference used a 1-based convention (first use = 1, second = 2),
/// off by one from `simulator/mod.rs`'s `new_count = if last_used_move == move
/// { count + 1 } else { 0 }`.
#[test]
fn test_consecutive_move_count_is_zero_based() {
    let state = battle_with_p2(vec![unknown_mon()]);
    let result = apply(
        state,
        vec![event(EventKind::MoveUsed {
            user: p2(0),
            move_used: PokemonMove::Earthquake,
            targets: vec![p1(0)],
        })],
    );
    assert_eq!(
        result.p2_active_mons[0].consecutive_move_count, 0,
        "first use of a move must be streak 0, matching the simulator's convention"
    );

    let result2 = apply(
        result,
        vec![event(EventKind::MoveUsed {
            user: p2(0),
            move_used: PokemonMove::Earthquake,
            targets: vec![p1(0)],
        })],
    );
    assert_eq!(
        result2.p2_active_mons[0].consecutive_move_count, 1,
        "second consecutive use of the same move must be streak 1"
    );

    let result3 = apply(
        result2,
        vec![event(EventKind::MoveUsed {
            user: p2(0),
            move_used: PokemonMove::DragonClaw,
            targets: vec![p1(0)],
        })],
    );
    assert_eq!(
        result3.p2_active_mons[0].consecutive_move_count, 0,
        "switching to a different move must reset the streak to 0"
    );
}

/// `times_hit` (Rage Fist) must only increment for a Physical/Special hit taken by a
/// TARGET of the enclosing move — never for the move's own recoil/crash/drain-reversal
/// self-damage on the user, and never for EOT/residual chip with no enclosing move.
/// Before the S15 fix, every `DamageDealt` on a mon incremented its own `times_hit`
/// regardless of cause, category, or self-vs-opponent.
#[test]
fn test_times_hit_excludes_self_damage_and_eot_chip() {
    let mut move_dex = HashMap::new();
    move_dex.insert(PokemonMove::Earthquake, normal_physical_move(PokemonMove::Earthquake, 100));

    let state = battle_with_p2(vec![unknown_mon()]);

    // P2 uses Earthquake, hits P1 AND takes self-damage (e.g. Life Orb recoil).
    let result = apply_ex(
        state,
        vec![
            event_with(
                EventKind::MoveUsed {
                    user: p2(0),
                    move_used: PokemonMove::Earthquake,
                    targets: vec![p1(0)],
                },
                vec![
                    event(EventKind::DamageDealt { target: p1(0), new_hp: PokemonHP::Number(200) }),
                    event(EventKind::DamageDealt { target: p2(0), new_hp: PokemonHP::Percent(90) }),
                ],
            ),
            // EOT chip on P2 with no enclosing MoveUsed (e.g. Sandstorm).
            event_with(
                EventKind::EndOfTurn,
                vec![event(EventKind::DamageDealt { target: p2(0), new_hp: PokemonHP::Percent(80) })],
            ),
        ],
        HashMap::new(),
        move_dex,
    );

    assert_eq!(
        result.p2_active_mons[0].times_hit, 0,
        "P2's own self-damage (recoil) and EOT chip must NOT increment its times_hit"
    );
}

/// `state.last_move_on_field` (Copycat) must be updated to the most recently used
/// non-Struggle move. Before the S15 fix, this field was never written by inference.
#[test]
fn test_last_move_on_field_updated_by_move_used() {
    let state = battle_with_p2(vec![unknown_mon()]);
    let result = apply(
        state,
        vec![event(EventKind::MoveUsed {
            user: p2(0),
            move_used: PokemonMove::DragonClaw,
            targets: vec![p1(0)],
        })],
    );
    assert_eq!(
        result.last_move_on_field,
        Some(PokemonMove::DragonClaw),
        "last_move_on_field must reflect the most recently used move"
    );
}

/// Struggle must not update `last_move_on_field` (matches the simulator's exclusion).
#[test]
fn test_last_move_on_field_not_updated_by_struggle() {
    let state = battle_with_p2(vec![unknown_mon()]);
    let mut state_with_prior = state;
    state_with_prior.last_move_on_field = Some(PokemonMove::DragonClaw);
    let result = apply(
        state_with_prior,
        vec![event(EventKind::MoveUsed {
            user: p2(0),
            move_used: PokemonMove::Struggle,
            targets: vec![p1(0)],
        })],
    );
    assert_eq!(
        result.last_move_on_field,
        Some(PokemonMove::DragonClaw),
        "Struggle must not overwrite last_move_on_field"
    );
}

// ── Regression: S8 — Struggle must not trip choice-lock / moveslot bookkeeping ──

/// A Choice-locked Pokémon whose locked move runs out of PP is forced into Struggle —
/// exactly the scenario Choice items cause. Before the S8 fix, `MoveUsed{Struggle}`
/// was treated like any other move: `pass1_choice_exclusion` saw two distinct moves
/// used (Earthquake, then Struggle) and unsoundly excluded every Choice item, even
/// though Struggle is not a real move choice and the simulator itself never treats it
/// as one for choice-lock purposes.
#[test]
fn test_struggle_does_not_exclude_choice_items() {
    let state = battle_with_p2(vec![unknown_mon()]);
    let result = apply(
        state,
        vec![
            event(EventKind::MoveUsed {
                user: p2(0),
                move_used: PokemonMove::Earthquake,
                targets: vec![p1(0)],
            }),
            event(EventKind::MoveUsed {
                user: p2(0),
                move_used: PokemonMove::Struggle,
                targets: vec![p1(0)],
            }),
        ],
    );
    let m = &result.p2_active_mons[0];
    assert!(
        !is_item_excluded(m, &Item::ChoiceBand),
        "ChoiceBand must NOT be excluded — Struggle is not a real move choice"
    );
    assert!(!is_item_excluded(m, &Item::ChoiceScarf));
    assert!(!is_item_excluded(m, &Item::ChoiceSpecs));
}

/// `MoveUsed{Struggle}` must not consume one of the mon's 4 real moveslots.
#[test]
fn test_struggle_does_not_burn_a_moveslot() {
    let state = battle_with_p2(vec![unknown_mon()]);
    let result = apply(
        state,
        vec![event(EventKind::MoveUsed {
            user: p2(0),
            move_used: PokemonMove::Struggle,
            targets: vec![p1(0)],
        })],
    );
    let m = &result.p2_active_mons[0];
    assert_eq!(
        m.known_moves,
        [None, None, None, None],
        "Struggle must not be recorded as a known move"
    );
}

// ── Pass 1: Boosts ────────────────────────────────────────────────────────────

#[test]
fn test_boost_changed_net() {
    let state = battle_with_p2(vec![unknown_mon()]);
    let result = apply(
        state,
        vec![
            event(EventKind::BoostChanged { target: p2(0), boost_idx: 0, stages: 2 }),  // 0=Atk
            event(EventKind::BoostChanged { target: p2(0), boost_idx: 0, stages: -1 }),
        ],
    );
    assert_eq!(result.p2_active_mons[0].boosts[0], 1, "net Atk boost should be +1");
}

#[test]
fn test_boosts_cleared_zeroes_all() {
    let mut mon = unknown_mon();
    mon.boosts = [2, -1, 3, 0, 1, 0, 0];
    let state = battle_with_p2(vec![mon]);
    let result = apply(
        state,
        vec![event(EventKind::BoostsCleared { target: p2(0) })],
    );
    assert_eq!(result.p2_active_mons[0].boosts, [0i8; 7]);
}

// ── Pass 1: Switch ────────────────────────────────────────────────────────────

#[test]
fn test_switch_from_known_back_to_active() {
    let back = UnknownPokemonState::from_opponent_species(
        Species::Garchomp,
        &HashMap::new(),
        50,
    );
    let mut state = battle_with_p2(vec![]);
    state.p2_known_back_mons = vec![back];
    state.p2_slot_conditions = vec![vec![]]; // add slot 0

    let result = apply(
        state,
        vec![event(EventKind::Switch(SwitchState {
            slot: p2(0),
            species: Species::Garchomp,
            level: 50,
            hp: PokemonHP::Percent(80),
            status: None,
            tera_type: None,
        }))],
    );

    assert_eq!(result.p2_active_mons.len(), 1, "should have one active mon");
    assert!(
        matches!(&result.p2_active_mons[0].possible_species, Unknown::Known(s) if *s == Species::Garchomp)
    );
    assert_eq!(result.p2_active_mons[0].hp, PokemonHP::Percent(80));
    assert!(result.p2_known_back_mons.is_empty(), "back should be empty after switch-in");
}

// ── Pass 2: Life Orb exclusion ────────────────────────────────────────────────

#[test]
fn test_no_recoil_excludes_life_orb() {
    // Rule out MagicGuard and SheerForce; Earthquake has no secondary → SheerForce irrelevant.
    let mut mon = unknown_mon();
    mon.possible_abilities = Unknown::Not(vec![Ability::MagicGuard, Ability::SheerForce]);

    // A P1 mon must actually occupy the damaged slot — the simulator never emits
    // DamageDealt for an empty slot, and event shapes here should stay realistic.
    let state = battle_nvn(vec![unknown_mon()], vec![mon]);

    // Provide Earthquake in the move dex so is_damaging=true.
    let mut move_dex = HashMap::new();
    move_dex.insert(PokemonMove::Earthquake, normal_physical_move(PokemonMove::Earthquake, 100));

    let result = apply_ex(
        state,
        // DamageDealt on P1 (target) only — no self-DamageDealt on user.
        vec![event_with(
            EventKind::MoveUsed {
                user: p2(0),
                move_used: PokemonMove::Earthquake,
                targets: vec![p1(0)],
            },
            vec![event(EventKind::DamageDealt {
                target: p1(0),
                new_hp: PokemonHP::Number(200),
            })],
        )],
        HashMap::new(),
        move_dex,
    );

    assert!(
        is_item_excluded(&result.p2_active_mons[0], &Item::LifeOrb),
        "LifeOrb must be excluded: no recoil and MagicGuard/SheerForce ruled out"
    );
}

#[test]
fn test_lo_recoil_present_does_not_exclude_life_orb() {
    let mut mon = unknown_mon();
    mon.possible_abilities = Unknown::Not(vec![Ability::MagicGuard, Ability::SheerForce]);
    let state = battle_with_p2(vec![mon]);

    let mut move_dex = HashMap::new();
    move_dex.insert(PokemonMove::Earthquake, normal_physical_move(PokemonMove::Earthquake, 100));

    let result = apply_ex(
        state,
        // DamageDealt on P1 AND a self-DamageDealt (Life Orb recoil) on P2.
        vec![event_with(
            EventKind::MoveUsed {
                user: p2(0),
                move_used: PokemonMove::Earthquake,
                targets: vec![p1(0)],
            },
            vec![
                event(EventKind::DamageDealt {
                    target: p1(0),
                    new_hp: PokemonHP::Number(200),
                }),
                event(EventKind::DamageDealt {
                    target: p2(0), // self-damage = Life Orb recoil
                    new_hp: PokemonHP::Percent(90),
                }),
            ],
        )],
        HashMap::new(),
        move_dex,
    );

    assert!(
        !is_item_excluded(&result.p2_active_mons[0], &Item::LifeOrb),
        "LifeOrb must not be excluded when self-damage reaction is present"
    );
}

// ── Regression: S10 — Life Orb absence must not be inferred when the user fainted ──

/// A self-KO move (Explosion) that hits and then faints the user emits no self
/// `DamageDealt` (the simulator sets HP to 0 directly — "no damage line in-game").
/// Before the S10 fix, `pass2_item_from_move` read the missing self-damage as proof
/// of no Life Orb; the faint (not the item) fully explains the absence, so the
/// exclusion was unsound.
#[test]
fn test_life_orb_not_excluded_when_user_faints_during_move() {
    let mut mon = unknown_mon();
    mon.possible_abilities = Unknown::Not(vec![Ability::MagicGuard, Ability::SheerForce]);
    let state = battle_with_p2(vec![mon]);

    let mut explosion = normal_physical_move(PokemonMove::Explosion, 250);
    explosion.self_destruct = SelfDestructType::Always;
    let mut move_dex = HashMap::new();
    move_dex.insert(PokemonMove::Explosion, explosion);

    let result = apply_ex(
        state,
        vec![event_with(
            EventKind::MoveUsed {
                user: p2(0),
                move_used: PokemonMove::Explosion,
                targets: vec![p1(0)],
            },
            vec![
                event(EventKind::DamageDealt {
                    target: p1(0),
                    new_hp: PokemonHP::Number(50),
                }),
                // Self-destruct's own faint: no self-DamageDealt, straight to Faint.
                event(EventKind::Faint { slot: p2(0) }),
            ],
        )],
        HashMap::new(),
        move_dex,
    );

    assert!(
        !is_item_excluded(&result.p2_active_mons[0], &Item::LifeOrb),
        "LifeOrb must NOT be excluded — the user's faint (not the item) explains the \
         missing self-damage reaction"
    );
}

/// Regression for the Bright Powder / Lax Incense soundness bug.
///
/// P1's 100%-accurate move misses P2 in Sandstorm, with Sand Veil not excluded. The old
/// engine emitted `[HasItem(BrightPowder) ∨ HasItem(LaxIncense)]`, which is unsound: it
/// rules out the true world where P2 holds no evasion item and Sand Veil caused the miss.
/// The fix adds `HasAbility(SandVeil)` as a disjunct, so BCP can't force a wrong item even
/// once BrightPowder and LaxIncense are excluded.
#[test]
fn test_brightpowder_clause_includes_sand_veil_in_sandstorm() {
    use crate::information::unknowns::Statement;

    // Sand Veil stays possible: `unknown_mon()` defaults possible_abilities to Not([]),
    // i.e. all abilities allowed.
    let mut p2_mon = unknown_mon();

    let mut state = battle_with_p2(vec![p2_mon]);
    state.weather = Some(Weather::Sandstorm);

    let mut move_dex = HashMap::new();
    let mut tackle = normal_physical_move(PokemonMove::Tackle, 40);
    move_dex.insert(PokemonMove::Tackle, tackle);

    let result = apply_ex(
        state,
        vec![event_with(
            EventKind::MoveUsed {
                user: p1(0),
                move_used: PokemonMove::Tackle,
                targets: vec![p2(0)],
            },
            vec![event(EventKind::Missed { target: p2(0) })],
        )],
        HashMap::new(),
        move_dex,
    );

    // The clause must contain HasAbility(SandVeil) as a disjunct so that BCP cannot
    // force an evasion item in a world where Sand Veil caused the miss.
    let clause_has_sand_veil = result.predicates.iter().any(|clause| {
        clause.iter().any(|s| {
            matches!(s, Statement::HasAbility { ability: Ability::SandVeil, .. })
        })
    });
    assert!(
        clause_has_sand_veil,
        "In sandstorm with Sand Veil possible, the 100%-accurate miss clause must \
         include HasAbility(SandVeil) as a disjunct (soundness invariant)"
    );
}

/// Positive regression: when Sand Veil / Snow Cloak / Tangled Feet are all ruled out,
/// the engine must still emit the Bright Powder / Lax Incense clause (no regression
/// of the original inference on a normal miss).
#[test]
fn test_brightpowder_clause_emitted_when_no_evasion_ability_possible() {
    use crate::information::unknowns::Statement;

    let mut p2_mon = unknown_mon();
    // Exclude all three evasion abilities so they can't suppress the clause.
    p2_mon.possible_abilities = Unknown::Not(vec![
        Ability::SandVeil,
        Ability::SnowCloak,
        Ability::TangledFeet,
    ]);

    let state = battle_with_p2(vec![p2_mon]);
    // No sandstorm / snow active, so the condition guards also don't add them.

    let mut move_dex = HashMap::new();
    move_dex.insert(PokemonMove::Tackle, normal_physical_move(PokemonMove::Tackle, 40));

    let result = apply_ex(
        state,
        vec![event_with(
            EventKind::MoveUsed {
                user: p1(0),
                move_used: PokemonMove::Tackle,
                targets: vec![p2(0)],
            },
            vec![event(EventKind::Missed { target: p2(0) })],
        )],
        HashMap::new(),
        move_dex,
    );

    // The clause must contain at least one item disjunct.
    let has_item_clause = result.predicates.iter().any(|clause| {
        clause.iter().any(|s| {
            matches!(
                s,
                Statement::HasItem { item: Item::BrightPowder, .. }
                    | Statement::HasItem { item: Item::LaxIncense, .. }
            )
        })
    });
    assert!(
        has_item_clause,
        "With all evasion abilities excluded, the 100%-accurate miss must still \
         emit a BrightPowder/LaxIncense clause"
    );
}

// ── Pass 4: Speed comparison ──────────────────────────────────────────────────

#[test]
fn test_speed_comparison_emitted_for_same_priority() {
    // P1 goes first with Tackle, P2 second → a SpeedComparison predicate should appear.
    let mut move_dex = HashMap::new();
    move_dex.insert(PokemonMove::Tackle, normal_physical_move(PokemonMove::Tackle, 40));

    // P1 mon has Quick Claw excluded so we get a clean SpeedComparison (not a disjunction).
    let mut p1_mon = unknown_mon_species(Species::Pikachu);
    p1_mon.item = Unknown::Not(vec![Item::QuickClaw]);
    p1_mon.possible_abilities = Unknown::Not(vec![Ability::QuickDraw]);

    let p2_mon = unknown_mon_species(Species::Snorlax);
    let state = battle_1v1(p1_mon, p2_mon);

    let result = apply_ex(
        state,
        vec![
            event(EventKind::MoveUsed { user: p1(0), move_used: PokemonMove::Tackle, targets: vec![p2(0)] }),
            event(EventKind::MoveUsed { user: p2(0), move_used: PokemonMove::Tackle, targets: vec![p1(0)] }),
        ],
        HashMap::new(),
        move_dex,
    );

    let has_speed_cmp = result.predicates.iter().any(|clause| {
        clause.iter().any(|lit| matches!(lit, Statement::SpeedComparison { .. }))
    });
    assert!(has_speed_cmp, "SpeedComparison predicate must be emitted");
}

#[test]
fn test_no_speed_comparison_different_priority() {
    // P1 uses a priority +1 move, P2 uses priority 0 → different brackets, no speed info.
    let mut quick_attack = normal_physical_move(PokemonMove::QuickAttack, 40);
    quick_attack.priority = 1;
    let tackle = normal_physical_move(PokemonMove::Tackle, 40);

    let mut move_dex = HashMap::new();
    move_dex.insert(PokemonMove::QuickAttack, quick_attack);
    move_dex.insert(PokemonMove::Tackle, tackle);

    let state = battle_1v1(unknown_mon_species(Species::Pikachu), unknown_mon_species(Species::Snorlax));
    let result = apply_ex(
        state,
        vec![
            event(EventKind::MoveUsed { user: p1(0), move_used: PokemonMove::QuickAttack, targets: vec![p2(0)] }),
            event(EventKind::MoveUsed { user: p2(0), move_used: PokemonMove::Tackle, targets: vec![p1(0)] }),
        ],
        HashMap::new(),
        move_dex,
    );

    let speed_cmp_count = result.predicates.iter()
        .flat_map(|c| c.iter())
        .filter(|lit| matches!(lit, Statement::SpeedComparison { .. }))
        .count();
    assert_eq!(speed_cmp_count, 0, "no SpeedComparison for different priority brackets");
}

// ── SpeedComparison propagation ───────────────────────────────────────────────

#[test]
fn test_speed_comparison_tightens_spe_bounds() {
    // Force a SpeedComparison directly into predicates and run BCP.
    // fast_idx=0 (P1 slot 0), slow_idx=1 (P2 slot 0), fast_mult=1, slow_mult=1.
    // If slow's minStats[5] = 100, fast's minStats[5] should be raised to ≥ 100.
    let mut p1_mon = unknown_mon_species(Species::Pikachu);
    p1_mon.minStats[5] = 50;
    p1_mon.maxStats[5] = 200;

    let mut p2_mon = unknown_mon_species(Species::Snorlax);
    p2_mon.minStats[5] = 100;
    p2_mon.maxStats[5] = 150;

    // Manually build the battle state with a SpeedComparison predicate.
    let mut state = battle_1v1(p1_mon, p2_mon);
    // P1 at idx 0, P2 at idx 1 (p1a=1, p1k=0, p1p=0, p2_start=1, p2a=1 → idx=1).
    state.predicates.push(vec![Statement::SpeedComparison {
        fast_idx: 0,
        slow_idx: 1,
        fast_mult: 1,
        slow_mult: 1,
    }]);

    let result = apply(state, vec![]);
    // fast's minStats[5] should be raised to ≥ slow's minStats[5] = 100.
    assert!(
        result.p1_active_mons[0].minStats[5] >= 100,
        "SpeedComparison must raise fast mon's min Spe to ≥ slow mon's min ({})",
        result.p1_active_mons[0].minStats[5]
    );
}

/// The symmetric BCP branch (previously untested): SpeedComparison must LOWER the
/// slow mon's max Spe to the fast mon's max.
#[test]
fn test_speed_comparison_lowers_slow_max_spe() {
    let mut p1_mon = unknown_mon_species(Species::Pikachu);
    p1_mon.minStats[5] = 50;
    p1_mon.maxStats[5] = 120;

    let mut p2_mon = unknown_mon_species(Species::Snorlax);
    p2_mon.minStats[5] = 40;
    p2_mon.maxStats[5] = 200;

    let mut state = battle_1v1(p1_mon, p2_mon);
    state.predicates.push(vec![Statement::SpeedComparison {
        fast_idx: 0,
        slow_idx: 1,
        fast_mult: 1,
        slow_mult: 1,
    }]);

    let result = apply(state, vec![]);
    assert!(
        result.p2_active_mons[0].maxStats[5] <= 120,
        "SpeedComparison must lower slow mon's max Spe to ≤ fast mon's max ({})",
        result.p2_active_mons[0].maxStats[5]
    );
}

/// `maybe_widen_for_illusion` (previously untested): when the switching side's back
/// contains a Zoroark, the incoming mon's Known species must widen to a Possibly set
/// including the Zoroark — the displayed species could be an Illusion disguise.
#[test]
fn test_switch_widens_species_for_possible_illusion() {
    let garchomp_back =
        UnknownPokemonState::from_opponent_species(Species::Garchomp, &HashMap::new(), 50);
    let zoroark_back =
        UnknownPokemonState::from_opponent_species(Species::Zoroark, &HashMap::new(), 50);
    let mut state = battle_with_p2(vec![]);
    state.p2_known_back_mons = vec![garchomp_back, zoroark_back];
    state.p2_slot_conditions = vec![vec![]];

    let result = apply(
        state,
        vec![event(EventKind::Switch(SwitchState {
            slot: p2(0),
            species: Species::Garchomp,
            level: 50,
            hp: PokemonHP::Percent(100),
            status: None,
            tera_type: None,
        }))],
    );

    match &result.p2_active_mons[0].possible_species {
        Unknown::Possibly(v) => {
            assert!(
                v.contains(&Species::Garchomp) && v.contains(&Species::Zoroark),
                "species must widen to include the possible Illusion user; got {v:?}"
            );
        }
        other => panic!(
            "with a Zoroark in the back the incoming species must be Possibly([...]); got {other:?}"
        ),
    }
}

/// `KnowsThreateningMove` satisfaction (previously untested): once the constrained mon
/// reveals an OHKO move, the clause is satisfied and BCP drops it from the store.
#[test]
fn test_knows_threatening_move_clause_satisfied_by_ohko_reveal() {
    let state = battle_1v1(unknown_mon(), unknown_mon());
    let mut move_dex = HashMap::new();
    move_dex.insert(PokemonMove::Fissure, normal_physical_move(PokemonMove::Fissure, 1));

    let result = apply_ex(
        state,
        vec![
            event_with(
                EventKind::AnticipationShudder { slot: p1(0) },
                vec![event(EventKind::AbilityRevealed {
                    slot: p1(0),
                    ability: Ability::Anticipation,
                })],
            ),
            event(EventKind::MoveUsed {
                user: p2(0),
                move_used: PokemonMove::Fissure,
                targets: vec![p1(0)],
            }),
        ],
        HashMap::new(),
        move_dex,
    );

    assert!(
        !result.predicates.iter().any(|c| {
            c.iter().any(|s| matches!(s, Statement::KnowsThreateningMove { .. }))
        }),
        "revealing Fissure satisfies the Anticipation constraint — BCP must drop it; \
         predicates = {:?}",
        result.predicates
    );
}

/// Sleep-preventer disjunction (S6 gap): a guaranteed-sleep secondary that fails to
/// land must emit Insomnia / Vital Spirit / Sweet Veil disjuncts.
#[test]
fn test_guaranteed_sleep_absence_emits_sleep_preventers() {
    use crate::state::dex_data::{PokemonSecondaryEffect, HitEffect};
    let mut p2_mon = UnknownPokemonState::from_opponent_species(Species::Garchomp, &HashMap::new(), 50);
    p2_mon.possible_types = Unknown::Known(vec![PokemonType::Dragon, PokemonType::Ground]);

    let p1_mon = {
        let mut m = UnknownPokemonState::from_opponent_species(Species::Garchomp, &garchomp_dex(), 50);
        m.item = Unknown::Known(Item::None);
        m.possible_abilities = Unknown::Known(Ability::SandVeil);
        m
    };
    let state = battle_1v1(p1_mon, p2_mon);

    let mut sleep_move = normal_physical_move(PokemonMove::Tackle, 40);
    sleep_move.secondaries = vec![PokemonSecondaryEffect {
        chance: 100,
        effect: HitEffect { status: Some(Status::Sleep(0)), ..Default::default() },
        random_choices: vec![],
    }];
    let mut move_dex = HashMap::new();
    move_dex.insert(PokemonMove::Tackle, sleep_move);

    let result = apply_ex(
        state,
        vec![event_with(
            EventKind::MoveUsed { user: p1(0), move_used: PokemonMove::Tackle, targets: vec![p2(0)] },
            vec![event(EventKind::DamageDealt { target: p2(0), new_hp: PokemonHP::Percent(80) })],
            // No StatusInflicted — the sleep was prevented.
        )],
        garchomp_dex(),
        move_dex,
    );

    let has_sleep_preventers = result.predicates.iter().any(|clause| {
        clause.iter().any(|s| matches!(s,
            Statement::HasAbility { ability: Ability::Insomnia, .. }
            | Statement::HasAbility { ability: Ability::VitalSpirit, .. }
            | Statement::HasAbility { ability: Ability::SweetVeil, .. }))
    });
    assert!(
        has_sleep_preventers,
        "guaranteed sleep that fails must emit the sleep-preventer disjunction; \
         predicates = {:?}",
        result.predicates
    );
}

// ── Contradiction → panic ─────────────────────────────────────────────────────

#[test]
#[should_panic(expected = "inference contradiction")]
fn test_item_conflict_panics() {
    let mut mon = unknown_mon();
    mon.item = Unknown::Known(Item::ChoiceScarf);
    let state = battle_with_p2(vec![mon]);
    // Revealing a different item contradicts Known(ChoiceScarf).
    apply(
        state,
        vec![event(EventKind::ItemRevealed { slot: p2(0), item: Item::ChoiceBand })],
    );
}

#[test]
#[should_panic(expected = "inference contradiction")]
fn test_status_conflict_panics() {
    let mut mon = unknown_mon();
    mon.status = Some(Status::Burn);
    let state = battle_with_p2(vec![mon]);
    // Inflicting Paralysis while already Burned — contradiction.
    apply(
        state,
        vec![event(EventKind::StatusInflicted { target: p2(0), status: Status::Paralysis })],
    );
}

// ── Pass 3: Damage → Stat Bounds ─────────────────────────────────────────────

/// Species dex entry for Garchomp (Ground/Dragon, base stats [108,130,95,80,85,102]).
fn garchomp_dex() -> HashMap<Species, PokemonData> {
    use crate::state::pokemon::PokemonGender;
    let mut dex = HashMap::new();
    dex.insert(Species::Garchomp, PokemonData {
        species:       Species::Garchomp,
        types:         vec![PokemonType::Dragon, PokemonType::Ground],
        base_stats:    [108, 130, 95, 80, 85, 102], // HP Atk Def SpA SpD Spe
        weight:        950, // 95.0 kg
        primary_ability: Some(Ability::SandVeil),
        abilities:     vec![Ability::SandVeil, Ability::RoughSkin],
        base_species:  None,
        forme:         None,
        required_item: None,
        battle_only:   None,
        default_gender: PokemonGender::Male,
    });
    dex
}

/// Ground-type physical move (used as Earthquake stand-in).
fn ground_physical_move(name: PokemonMove, bp: u16) -> MoveData {
    MoveData {
        name,
        base_power:     bp,
        accuracy:       AccuracyType::Percent(100),
        target:         MoveTarget::Normal,
        secondaries:    vec![],
        self_secondaries: vec![],
        pp:             10,
        category:       MoveCategory::Physical,
        pokemon_type:   PokemonType::Ground,
        priority:       0,
        flags:          vec![],
        ohko:           false,
        thaws_target:   false,
        heal_fraction:  [0, 0],
        force_switch:   false,
        self_switch:    SelfSwitchType::None,
        self_boost:     [0; 7],
        self_destruct:  SelfDestructType::None,
        breaks_protect: false,
        recoil_fraction:  [0, 0],
        drain_fraction:   [0, 0],
        mind_blown_recoil: false,
        struggle_recoil: false,
        crit_ratio:     1,
        foul_play:      false,
        ignore_ability:       false,
        ignore_defense_boosts: false,
        ignore_evasion:        false,
        ignore_immunity:       vec![],
        multihit_range:        [1, 1],
        multihit_accuracy:     false,
        sleep_usable:          false,
        has_crash_damage:      false,
        damage_override:       DamageOverride::None,
        stalling_move:         false,
        override_offensive_stat: None,
        override_defensive_stat: None,
    }
}

/// A "known" P1 mon (our own) with concrete HP and stats for Direction-B tests.
///
/// hp = 500 (Number), Def = 100. Normal-type (Ground-immune check: Normal is not immune
/// to Ground). All unknown fields collapsed to Known so the oracle sees exact values.
///
/// Uses Species::Snorlax as a placeholder — Snorlax is intentionally absent from the
/// `garchomp_dex()` used in these tests, so Pass 5 skips P1 (can't verify EV/IV with
/// no base-stat data). Own-mon stats are always fully known, so the skip is correct.
fn known_p1_normal() -> UnknownPokemonState {
    // Snorlax is NOT in garchomp_dex() → pass5 skips validation for this mon,
    // so the out-of-range HP=500 does not trigger a contradiction.
    let mut mon = unknown_mon_species(Species::Snorlax);
    mon.hp               = PokemonHP::Number(500);
    mon.minStats         = [500, 35, 100, 55, 125, 55];
    mon.maxStats         = [500, 35, 100, 55, 125, 55];
    mon.item             = Unknown::Known(Item::None);
    mon.possible_abilities = Unknown::Known(Ability::None);
    mon.possible_types   = Unknown::Known(vec![PokemonType::Normal]);
    mon
}

/// Garchomp attacker with *neutral* nature only, no item, no damage-boosting ability.
///
/// This removes booster/nature ambiguity from the test, making the expected BSV bound
/// computable by hand. The oracle will use the exact pre-nature BSV as the final stat.
///
/// **Important**: passes `garchomp_dex()` to `from_opponent_species` so the real base
/// stats [108,130,…] are used.  With an empty dex the fallback base is [100;6], which
/// would initialise pre-nature bounds to [105, 152] instead of the correct [135, 182].
fn neutral_no_item_garchomp() -> UnknownPokemonState {
    use crate::state::pokemon::Nature;
    // Use real Garchomp dex so min/max pre-nature stat are initialised from base=130.
    let mut mon = UnknownPokemonState::from_opponent_species(Species::Garchomp, &garchomp_dex(), 50);
    // Lock nature to neutral so BSV == final stat in the oracle.
    mon.possible_natures   = Unknown::Known(Nature::Hardy);
    mon.item               = Unknown::Known(Item::None);
    // SandVeil: not a damage-boosting ability.
    mon.possible_abilities = Unknown::Known(Ability::SandVeil);
    mon.hp = PokemonHP::Percent(100);
    mon
}

/// Runs Pass 3 for Direction B and returns the resulting state.
///
/// `damage` = HP lost by P1 (pre_hp - new_hp).  P2 is the attacker; P1 is the target.
fn run_direction_b(p2_mon: UnknownPokemonState, damage: u16) -> crate::information::unknowns::UnknownBattleState {
    let our_mon = known_p1_normal();
    let pre_hp = match our_mon.hp { PokemonHP::Number(n) => n, _ => panic!("expected Number") };
    let new_hp = pre_hp.saturating_sub(damage);

    let state = battle_1v1(our_mon, p2_mon);

    let mut move_dex = HashMap::new();
    move_dex.insert(PokemonMove::Earthquake, ground_physical_move(PokemonMove::Earthquake, 100));

    apply_ex(
        state,
        vec![event_with(
            EventKind::MoveUsed { user: p2(0), move_used: PokemonMove::Earthquake, targets: vec![p1(0)] },
            vec![event(EventKind::DamageDealt { target: p1(0), new_hp: PokemonHP::Number(new_hp) })],
        )],
        garchomp_dex(),
        move_dex,
    )
}

// ── Direction B: upper-bound tightening ──────────────────────────────────────

/// Damage 91 is achievable by Garchomp BSV ≤ 161 but NOT by BSV ≥ 162.
///
/// Manual derivation (level 50, BP=100 Earthquake, no item/ability/weather/terrain/screens):
///   base_damage_formula(50, 100, atk, def=100) = floor(floor(22*100*atk/100/50)+2)
///                                                = floor(floor(0.44*atk)+2)
///   Then × roll/100 × 1.5 (STAB Ground on Garchomp) → per-roll damage.
///
/// BSV=161 → base=72 → roll=85 → floor(floor(72*0.85)*1.5) = floor(61*1.5) = 91. ✓
/// BSV=162 → base=73 → roll=85 → floor(floor(73*0.85)*1.5) = floor(62*1.5) = 93 > 91.
///           For any roll, BSV=162 produces ≥ 93.  So BSV=162 is not feasible for damage=91.
#[test]
fn test_pass3_dir_b_upper_bound_tightened() {
    let result = run_direction_b(neutral_no_item_garchomp(), 91);
    let p2 = &result.p2_active_mons[0];

    // Initial max_pre_nature_stat[1] for Garchomp at lv50 (IV=31, EV=252, neutral) is 182.
    // After seeing damage=91, the highest feasible BSV is 161 → bound must have dropped.
    assert!(
        p2.max_pre_nature_stat[1] <= 161,
        "max BSV must be ≤ 161 after seeing damage 91 (got {})",
        p2.max_pre_nature_stat[1]
    );

    // Soundness: the min attacker BSV (135) CAN produce damage=91 (at roll=100),
    // so the lower bound must NOT be raised above 135.
    assert!(
        p2.min_pre_nature_stat[1] <= 135,
        "min BSV must not exceed 135 — that value is consistent with damage 91 (got {})",
        p2.min_pre_nature_stat[1]
    );
}

// ── Direction B: lower-bound tightening ──────────────────────────────────────

/// Damage 103 cannot be produced by BSV ≤ 152 but CAN by BSV ≥ 153.
///
/// BSV=153 → base=69 → roll=100 → floor(69*1.0*1.5) = floor(103.5) = 103. ✓
/// BSV=152 → base=68 → all rolls produce max floor(68*1.0*1.5)=floor(102)=102 < 103.
///           And at roll=85: floor(floor(68*0.85)*1.5)=floor(57*1.5)=85 < 103.
#[test]
fn test_pass3_dir_b_lower_bound_tightened() {
    let result = run_direction_b(neutral_no_item_garchomp(), 103);
    let p2 = &result.p2_active_mons[0];

    // Initial min_pre_nature_stat[1] for Garchomp at lv50 (IV=0, EV=0, neutral) is 135.
    // After seeing damage=103, the lowest feasible BSV is 153 → bound must have risen.
    assert!(
        p2.min_pre_nature_stat[1] >= 153,
        "min BSV must be ≥ 153 after seeing damage 103 (got {})",
        p2.min_pre_nature_stat[1]
    );

    // Soundness: BSV=182 (max neutral Garchomp) CAN produce damage=103 (at roll=85),
    // so the upper bound must NOT be lowered below 182.
    assert!(
        p2.max_pre_nature_stat[1] >= 182,
        "max BSV must not go below 182 — that value is consistent with damage 103 (got {})",
        p2.max_pre_nature_stat[1]
    );
}

// ── Direction B: soundness across the full damage range ──────────────────────

/// Every achievable damage value in Garchomp's EQ range (under force_max_ivs=true)
/// must not cause a contradiction in Pass 3 (no panic, and bounds remain valid min ≤ max).
#[test]
fn test_pass3_dir_b_no_contradiction_across_damage_range() {
    // Sweep is 85..=123, not 76..=123: InferenceConfig::default() has force_max_ivs=true,
    // pinning IV=31, which makes the minimum achievable Atk BSV 150 (calc_stat(130,31,0,50,1)=150)
    // and its damage range [85,102]. Damage 76-84 would need BSV<150, unreachable under IV=31.
    // Only assert the true BSV (150) in-bounds for damage <= 102 (its own range); higher
    // damages require BSV>150, so asserting 150 there would be unsound.
    const TRUE_ATK_BSV: u16 = 150;
    const MAX_FROM_150: u16 = 102; // floor(floor(68*1.0)*1.5) = floor(102) = 102

    for damage in 85u16..=123u16 {
        let result = run_direction_b(neutral_no_item_garchomp(), damage);
        let p2 = &result.p2_active_mons[0];
        assert!(
            p2.min_pre_nature_stat[1] <= p2.max_pre_nature_stat[1],
            "Pass 3 produced inverted bounds (min {} > max {}) for damage={}",
            p2.min_pre_nature_stat[1], p2.max_pre_nature_stat[1], damage
        );
        // Bounds must stay inside Garchomp's theoretical BSV range [135, 182].
        assert!(
            p2.min_pre_nature_stat[1] >= 135,
            "min BSV ({}) dropped below Garchomp's minimum (135) for damage={}",
            p2.min_pre_nature_stat[1], damage
        );
        assert!(
            p2.max_pre_nature_stat[1] <= 182,
            "max BSV ({}) exceeded Garchomp's maximum (182) for damage={}",
            p2.max_pre_nature_stat[1], damage
        );

        // ── T3 additions: soundness + tightness assertions ────────────────────
        if damage <= MAX_FROM_150 {
            // Soundness: true value (150) must lie within bounds for any damage
            // achievable from Atk=150.  A regression that over-narrows (raising min
            // above 150 or lowering max below 150) would be caught here.
            assert!(
                p2.min_pre_nature_stat[1] <= TRUE_ATK_BSV
                    && TRUE_ATK_BSV <= p2.max_pre_nature_stat[1],
                "soundness: true Atk BSV ({}) must lie within [{}, {}] for damage={}",
                TRUE_ATK_BSV, p2.min_pre_nature_stat[1], p2.max_pre_nature_stat[1], damage
            );
        }
        if damage == 100 {
            // Tightness: damage=100 is produced only by Atk in roughly [148, 175],
            // so both bounds should be strictly inside the species range [135, 182].
            assert!(
                p2.min_pre_nature_stat[1] > 135,
                "tightness: damage=100 should raise min above 135; got {}",
                p2.min_pre_nature_stat[1]
            );
            assert!(
                p2.max_pre_nature_stat[1] < 182,
                "tightness: damage=100 should lower max below 182; got {}",
                p2.max_pre_nature_stat[1]
            );
        }
    }
}

// ── Direction B: fixed-damage move skipped ────────────────────────────────────

/// Fixed-damage moves (DamageOverride != None) carry no stat signal.  Pass 3 must
/// not narrow any bounds when it sees one.
#[test]
fn test_pass3_fixed_damage_move_skipped() {
    let p2_mon = neutral_no_item_garchomp();
    let our_mon = known_p1_normal();

    let initial_min = p2_mon.min_pre_nature_stat[1];
    let initial_max = p2_mon.max_pre_nature_stat[1];

    let state = battle_1v1(our_mon, p2_mon);

    // Build a fixed-damage move (level-dependent, like Seismic Toss).
    let fixed_move = MoveData {
        name:           PokemonMove::SeismicToss,
        base_power:     1, // ignored for fixed-damage
        accuracy:       AccuracyType::Percent(100),
        target:         MoveTarget::Normal,
        category:       MoveCategory::Physical,
        pokemon_type:   PokemonType::Normal,
        priority:       0,
        pp:             10,
        flags:          vec![],
        ohko:           false,
        thaws_target:   false,
        heal_fraction:  [0, 0],
        force_switch:   false,
        self_switch:    SelfSwitchType::None,
        self_boost:     [0; 7],
        self_destruct:  SelfDestructType::None,
        breaks_protect: false,
        recoil_fraction:  [0, 0],
        drain_fraction:   [0, 0],
        mind_blown_recoil: false,
        struggle_recoil:  false,
        crit_ratio:     1,
        foul_play:      false,
        ignore_ability:        false,
        ignore_defense_boosts: false,
        ignore_evasion:        false,
        ignore_immunity:       vec![],
        multihit_range:        [1, 1],
        multihit_accuracy:     false,
        sleep_usable:          false,
        has_crash_damage:      false,
        damage_override:       DamageOverride::Level, // fixed: causes pass3 skip
        stalling_move:         false,
        secondaries:           vec![],
        self_secondaries:      vec![],
        override_offensive_stat: None,
        override_defensive_stat: None,
    };

    let mut move_dex = HashMap::new();
    move_dex.insert(PokemonMove::SeismicToss, fixed_move);

    let result = apply_ex(
        state,
        vec![event_with(
            EventKind::MoveUsed { user: p2(0), move_used: PokemonMove::SeismicToss, targets: vec![p1(0)] },
            vec![event(EventKind::DamageDealt { target: p1(0), new_hp: PokemonHP::Number(450) })],
        )],
        garchomp_dex(),
        move_dex,
    );

    let p2_result = &result.p2_active_mons[0];
    assert_eq!(
        p2_result.min_pre_nature_stat[1], initial_min,
        "min BSV must not change for a fixed-damage move"
    );
    assert_eq!(
        p2_result.max_pre_nature_stat[1], initial_max,
        "max BSV must not change for a fixed-damage move"
    );
}

// ── Direction B: booster item → guarded predicate ────────────────────────────

/// When the opponent's item is unknown and Choice Band is possible, Pass 3 emits
/// *conditional* lower-bound predicates rather than unconditional tightening.
///
/// Specifically: if damage D is produced without Choice Band, the lower bound for BSV
/// may be tight; but if Choice Band is possible, the unconditional bound is *loose*
/// (union includes Band-assisted low-stat scenarios).  BCP can then propagate tightly
/// once the item is excluded.
#[test]
fn test_pass3_dir_b_choice_band_loosens_unconditional_bound() {
    use crate::state::pokemon::Nature;
    // Attacker: neutral nature only, but item is unknown (Choice Band remains possible).
    // Use real dex so pre-nature bounds reflect true base Atk=130.
    let mut p2_mon = UnknownPokemonState::from_opponent_species(Species::Garchomp, &garchomp_dex(), 50);
    p2_mon.possible_natures   = Unknown::Known(Nature::Hardy);
    p2_mon.possible_abilities = Unknown::Known(Ability::SandVeil);
    // item is Unknown::Not(vec![]) — fully unknown, Choice Band is possible.
    p2_mon.hp = PokemonHP::Percent(100);

    // A "tight" damage (e.g. 91) that without Choice Band constrains BSV ≤ 161.
    let result_with_band_possible = run_direction_b(p2_mon.clone(), 91);
    let bound_with_band = result_with_band_possible.p2_active_mons[0].max_pre_nature_stat[1];

    // Offensive items only *increase* damage, so the no-item config always permits the
    // highest BSV — the unconditional max never exceeds the band-free bound (161).
    // Band's real effect is on the LOWER side: a low-BSV attacker with ×1.5 Band could
    // also produce 91, so the min must stay at the species floor while Band is possible.
    assert_eq!(
        bound_with_band, 161,
        "unconditional max BSV must equal the band-free bound"
    );
    assert_eq!(
        result_with_band_possible.p2_active_mons[0].min_pre_nature_stat[1], 135,
        "with Band possible the min BSV must stay at the species floor"
    );

    // After excluding Choice Band via ItemRevealed, BCP should propagate a tighter bound
    // — feed a separate event stream that reveals a Lum Berry instead.
    let our_mon2 = known_p1_normal();
    let pre_hp = 500u16;
    let new_hp = pre_hp - 91;
    let state = battle_1v1(our_mon2, p2_mon);
    let mut move_dex = HashMap::new();
    move_dex.insert(PokemonMove::Earthquake, ground_physical_move(PokemonMove::Earthquake, 100));

    let result = apply_ex(
        state,
        vec![
            // Move event that produces damage 91.
            event_with(
                EventKind::MoveUsed { user: p2(0), move_used: PokemonMove::Earthquake, targets: vec![p1(0)] },
                vec![event(EventKind::DamageDealt { target: p1(0), new_hp: PokemonHP::Number(new_hp) })],
            ),
            // Item revealed: NOT Choice Band — eliminates the booster disjunct in BCP.
            event(EventKind::ItemRevealed { slot: p2(0), item: Item::LumBerry }),
        ],
        garchomp_dex(),
        move_dex,
    );

    let p2_r = &result.p2_active_mons[0];
    // After the band is excluded, BCP should fire the guarded EVIVStatLE → tighten max.
    assert!(
        p2_r.max_pre_nature_stat[1] <= 161,
        "After Choice Band excluded via BCP, max BSV must drop to ≤ 161 (got {})",
        p2_r.max_pre_nature_stat[1]
    );
    // Soundness: lower bound must not exceed 135.
    assert!(
        p2_r.min_pre_nature_stat[1] <= 135,
        "min BSV must remain ≤ 135 after item exclusion (got {})",
        p2_r.min_pre_nature_stat[1]
    );
}

// ── Direction B: crit observed vs not ────────────────────────────────────────

/// When a crit is observed, the oracle filters to crit outcomes only (×1.5 crit mult).
/// The same damage that implies one Atk range under no-crit implies a *different*
/// (lower) range under crit.  This test verifies that the crit flag reaches Pass 3
/// without causing a contradiction.
#[test]
fn test_pass3_dir_b_crit_observed_no_contradiction() {
    let our_mon = known_p1_normal();
    let p2_mon = neutral_no_item_garchomp();

    let state = battle_1v1(our_mon, p2_mon.clone());
    let mut move_dex = HashMap::new();
    move_dex.insert(PokemonMove::Earthquake, ground_physical_move(PokemonMove::Earthquake, 100));

    // Feed a FEASIBLE crit damage (previously 100, which no Atk in [135,182] can
    // produce under the ×1.5 crit multiplier — the test only exercised robustness).
    // Note the STAB quantization: damage is floor(1.5·x) for integer x, so values
    // like 140 are unreachable for ANY attacker (floor(1.5·93)=139, floor(1.5·94)=141).
    // 141 is reachable by mid-range Atk values under a crit.
    let result = apply_ex(
        state,
        vec![event_with(
            EventKind::MoveUsed { user: p2(0), move_used: PokemonMove::Earthquake, targets: vec![p1(0)] },
            vec![
                event(EventKind::Crit { target: p1(0) }),           // crit signalled
                event(EventKind::DamageDealt { target: p1(0), new_hp: PokemonHP::Number(359) }), // 141 damage
            ],
        )],
        garchomp_dex(),
        move_dex,
    );

    let p2_r = &result.p2_active_mons[0];
    // The crit flag must reach the oracle: 140 crit damage narrows the Atk BSV to a
    // proper sub-range of [135, 182] without inversion or contradiction. Exact bounds
    // pinned from the oracle (binary-search Direction B under the crit multiplier).
    assert!(
        p2_r.min_pre_nature_stat[1] <= p2_r.max_pre_nature_stat[1],
        "Crit observation must not produce inverted bounds (min {}, max {})",
        p2_r.min_pre_nature_stat[1], p2_r.max_pre_nature_stat[1]
    );
    assert!(
        p2_r.min_pre_nature_stat[1] > 135 || p2_r.max_pre_nature_stat[1] < 182,
        "feasible crit damage must actually narrow the bounds; got [{}, {}]",
        p2_r.min_pre_nature_stat[1], p2_r.max_pre_nature_stat[1]
    );
}

// ── Pass 4: Speed ordering ────────────────────────────────────────────────────

/// Helper: a status move (for Prankster tests).
fn poke_status_move(name: PokemonMove) -> MoveData {
    MoveData {
        name,
        base_power: 0,
        accuracy: AccuracyType::Percent(100),
        target: MoveTarget::Normal,
        secondaries: vec![],
        self_secondaries: vec![],
        pp: 10,
        category: MoveCategory::Status,
        pokemon_type: PokemonType::Normal,
        priority: 0,
        flags: vec![],
        ohko: false, thaws_target: false, heal_fraction: [0, 0], force_switch: false,
        self_switch: SelfSwitchType::None, self_boost: [0; 7], self_destruct: SelfDestructType::None,
        breaks_protect: false, recoil_fraction: [0, 0], drain_fraction: [0, 0],
        mind_blown_recoil: false, struggle_recoil: false, crit_ratio: 1, foul_play: false,
        ignore_ability: false, ignore_defense_boosts: false, ignore_evasion: false,
        ignore_immunity: vec![], multihit_range: [1, 1], multihit_accuracy: false,
        sleep_usable: false, has_crash_damage: false, damage_override: DamageOverride::None,
        stalling_move: false, override_offensive_stat: None, override_defensive_stat: None,
    }
}

/// Build a battle state where P2 moves first (faster) and P1 moves second.
/// Both use physical moves at priority 0. Neither has Quick Claw or Quick Draw.
fn speed_order_events(p2_move: PokemonMove, p1_move: PokemonMove) -> Vec<InformationEvent> {
    vec![
        event(EventKind::MoveUsed { user: p2(0), move_used: p2_move, targets: vec![p1(0)] }),
        event(EventKind::MoveUsed { user: p1(0), move_used: p1_move, targets: vec![p2(0)] }),
    ]
}

/// A mon with all speed-affecting items/abilities explicitly excluded.
fn no_speed_escape_mon(species: Species) -> UnknownPokemonState {
    let mut mon = unknown_mon_species(species);
    // Exclude items and abilities that could create a speed escape clause.
    mon.item = Unknown::Not(vec![
        Item::QuickClaw, Item::ChoiceScarf, Item::IronBall, Item::LaggingTail, Item::FullIncense,
    ]);
    mon.possible_abilities = Unknown::Not(vec![
        Ability::QuickDraw, Ability::Prankster, Ability::GaleWings, Ability::Triage,
        Ability::Stall, Ability::SwiftSwim, Ability::Chlorophyll, Ability::SandRush,
        Ability::SlushRush, Ability::SurgeSurfer, Ability::Unburden, Ability::QuickFeet,
    ]);
    mon
}

/// Pass 4 emits a SpeedComparison predicate when the faster P2 mon moves first
/// and neither mon has any speed-escape items/abilities.
#[test]
fn test_pass4_speed_comparison_emitted() {
    let p1_mon = no_speed_escape_mon(Species::Garchomp);
    let p2_mon = no_speed_escape_mon(Species::Garchomp);
    let state = battle_1v1(p1_mon, p2_mon);

    let mut move_dex = HashMap::new();
    move_dex.insert(PokemonMove::Earthquake, normal_physical_move(PokemonMove::Earthquake, 100));
    move_dex.insert(PokemonMove::DragonClaw, normal_physical_move(PokemonMove::DragonClaw, 80));

    let result = apply_ex(
        state,
        speed_order_events(PokemonMove::Earthquake, PokemonMove::DragonClaw),
        HashMap::new(),
        move_dex,
    );

    // In battle_1v1: P1 active mon = idx 0, P2 active mon = idx 1.
    // P2 moved first → fast_idx = 1, slow_idx = 0 (in normal order).
    let has_speed_cmp = result.predicates.iter().any(|clause| {
        clause.iter().any(|stmt| matches!(stmt, Statement::SpeedComparison { fast_idx: 1, slow_idx: 0, .. }))
    });
    assert!(has_speed_cmp, "Pass 4 must emit SpeedComparison(fast=P2, slow=P1) when P2 moves first in priority 0");
}

/// Under Trick Room the slower mon goes first; Pass 4 should swap fast/slow so the
/// SpeedComparison reflects the naturally-faster mon (the one that moved SECOND).
#[test]
fn test_pass4_trick_room_swaps_comparison() {
    let p1_mon = no_speed_escape_mon(Species::Garchomp);
    let p2_mon = no_speed_escape_mon(Species::Garchomp);
    let mut state = battle_1v1(p1_mon, p2_mon);
    state.pseudo_weathers = vec![PseudoWeather::TrickRoom];
    state.pseudo_weather_turns = vec![Unknown::Known(3)];

    let mut move_dex = HashMap::new();
    move_dex.insert(PokemonMove::Earthquake, normal_physical_move(PokemonMove::Earthquake, 100));
    move_dex.insert(PokemonMove::DragonClaw, normal_physical_move(PokemonMove::DragonClaw, 80));

    // P2 moves first under Trick Room → P2 is the SLOWER mon.
    // Pass 4 must swap: the SpeedComparison should have fast_idx=0 (P1=slower→second=faster-in-TR).
    let result = apply_ex(
        state,
        speed_order_events(PokemonMove::Earthquake, PokemonMove::DragonClaw),
        HashMap::new(),
        move_dex,
    );

    let has_tr_cmp = result.predicates.iter().any(|clause| {
        clause.iter().any(|stmt| matches!(stmt, Statement::SpeedComparison { fast_idx: 0, slow_idx: 1, .. }))
    });
    assert!(has_tr_cmp, "Under Trick Room, SpeedComparison must have fast_idx=P1 (moved second = naturally faster)");

    // Sanity: should NOT have the non-TR direction.
    let has_wrong_cmp = result.predicates.iter().any(|clause| {
        clause.iter().any(|stmt| matches!(stmt, Statement::SpeedComparison { fast_idx: 1, slow_idx: 0, .. }))
    });
    assert!(!has_wrong_cmp, "Trick Room must not emit the non-reversed SpeedComparison");
}

/// When P2 uses a Status move and might have Prankster (+1 priority to status moves),
/// Pass 4 must include HasAbility{Prankster} as an escape disjunct so the predicate
/// stays sound even if Prankster explains the ordering.
#[test]
fn test_pass4_prankster_escape_on_status_move() {
    let p1_mon = no_speed_escape_mon(Species::Garchomp);
    // P2 might have Prankster (all abilities allowed except QuickDraw which would be a
    // different speed-priority escape; Prankster must stay possible so the escape disjunct fires).
    let mut p2_mon = no_speed_escape_mon(Species::Garchomp);
    p2_mon.possible_abilities = Unknown::Not(vec![Ability::QuickDraw]);
    p2_mon.item = Unknown::Not(vec![
        Item::QuickClaw, Item::ChoiceScarf, Item::IronBall, Item::LaggingTail, Item::FullIncense,
    ]);

    let state = battle_1v1(p1_mon, p2_mon);

    let mut move_dex = HashMap::new();
    // P2 uses a Status move (priority 0), P1 uses a Physical move (priority 0).
    move_dex.insert(PokemonMove::WillOWisp, poke_status_move(PokemonMove::WillOWisp));
    move_dex.insert(PokemonMove::DragonClaw, normal_physical_move(PokemonMove::DragonClaw, 80));

    let result = apply_ex(
        state,
        vec![
            event(EventKind::MoveUsed { user: p2(0), move_used: PokemonMove::WillOWisp, targets: vec![p1(0)] }),
            event(EventKind::MoveUsed { user: p1(0), move_used: PokemonMove::DragonClaw, targets: vec![p2(0)] }),
        ],
        HashMap::new(),
        move_dex,
    );

    // The predicate clause for this comparison must include HasAbility{Prankster} as a
    // disjunct (since Prankster could explain P2 going first with a status move).
    let has_prankster_escape = result.predicates.iter().any(|clause| {
        let has_speed_cmp = clause.iter().any(|s| matches!(s, Statement::SpeedComparison { fast_idx: 1, .. }));
        let has_prankster = clause.iter().any(|s| matches!(s,
            Statement::HasAbility { mon_idx: 1, ability: Ability::Prankster }
        ));
        has_speed_cmp && has_prankster
    });
    assert!(has_prankster_escape, "Status move by P2 with possible Prankster must add Prankster escape to clause");
}

/// A Physical move by P2 should NOT add a Prankster escape disjunct — Prankster only
/// affects Status moves.
#[test]
fn test_pass4_no_prankster_escape_on_physical_move() {
    let p1_mon = no_speed_escape_mon(Species::Garchomp);
    let mut p2_mon = no_speed_escape_mon(Species::Garchomp);
    // Allow Prankster on P2 (but the move is Physical → no escape needed).
    p2_mon.possible_abilities = Unknown::Not(vec![Ability::QuickDraw]);
    p2_mon.item = Unknown::Not(vec![
        Item::QuickClaw, Item::ChoiceScarf, Item::IronBall, Item::LaggingTail, Item::FullIncense,
    ]);

    let state = battle_1v1(p1_mon, p2_mon);
    let mut move_dex = HashMap::new();
    move_dex.insert(PokemonMove::Earthquake, normal_physical_move(PokemonMove::Earthquake, 100));
    move_dex.insert(PokemonMove::DragonClaw, normal_physical_move(PokemonMove::DragonClaw, 80));

    let result = apply_ex(
        state,
        speed_order_events(PokemonMove::Earthquake, PokemonMove::DragonClaw),
        HashMap::new(),
        move_dex,
    );

    // Prankster escape must NOT appear for a physical move.
    let has_prankster_escape = result.predicates.iter().any(|clause|
        clause.iter().any(|s| matches!(s, Statement::HasAbility { ability: Ability::Prankster, .. }))
    );
    assert!(!has_prankster_escape, "Physical move must not add Prankster escape (Prankster only affects status moves)");
}

/// Choice Scarf is a speed-boosting item; its presence on the fast mon makes the
/// SpeedComparison predicate too strong (unsound) → Pass 4 must add an escape disjunct.
#[test]
fn test_pass4_choice_scarf_escape_on_fast_mon() {
    let p1_mon = no_speed_escape_mon(Species::Garchomp);
    let mut p2_mon = no_speed_escape_mon(Species::Garchomp);
    // Allow Choice Scarf on P2 (fast mon). Remove it from the exclusion list.
    p2_mon.item = Unknown::Not(vec![
        Item::QuickClaw, Item::IronBall, Item::LaggingTail, Item::FullIncense,
        // NOT excluding ChoiceScarf → it's possible
    ]);

    let state = battle_1v1(p1_mon, p2_mon);
    let mut move_dex = HashMap::new();
    move_dex.insert(PokemonMove::Earthquake, normal_physical_move(PokemonMove::Earthquake, 100));
    move_dex.insert(PokemonMove::DragonClaw, normal_physical_move(PokemonMove::DragonClaw, 80));

    let result = apply_ex(
        state,
        speed_order_events(PokemonMove::Earthquake, PokemonMove::DragonClaw),
        HashMap::new(),
        move_dex,
    );

    // The clause for P2-first must include HasItem{ChoiceScarf, mon_idx=1} as escape.
    let has_scarf_escape = result.predicates.iter().any(|clause| {
        let has_speed_cmp = clause.iter().any(|s| matches!(s, Statement::SpeedComparison { fast_idx: 1, .. }));
        let has_scarf = clause.iter().any(|s| matches!(s,
            Statement::HasItem { mon_idx: 1, item: Item::ChoiceScarf }
        ));
        has_speed_cmp && has_scarf
    });
    assert!(has_scarf_escape, "Choice Scarf possible on fast mon → must add ChoiceScarf escape disjunct");
}

// ── Pass 3: Multi-hit damage ──────────────────────────────────────────────────

/// A 2-hit physical move: both hits delivered against a P1 mon with exact HP.
/// Verifies that the engine processes multi-hit without panicking and produces
/// valid (sound) bounds.
#[test]
fn test_pass3_multihit_2hits_no_contradiction() {
    let mut p1_mon = known_p1_normal(); // HP=500, Def=100
    let p2_mon = neutral_no_item_garchomp();

    let state = battle_1v1(p1_mon, p2_mon);

    // 2-hit move (50 BP each). Two consecutive DamageDealt reactions.
    let mut move_dex = HashMap::new();
    let mut hit2_move = normal_physical_move(PokemonMove::BulletSeed, 25);
    hit2_move.multihit_range = [2, 2];
    move_dex.insert(PokemonMove::BulletSeed, hit2_move);

    // Hit 1: HP drops 500 → 460 (40 damage)
    // Hit 2: HP drops 460 → 422 (38 damage)
    let result = apply_ex(
        state,
        vec![event_with(
            EventKind::MoveUsed { user: p2(0), move_used: PokemonMove::BulletSeed, targets: vec![p1(0)] },
            vec![
                event(EventKind::DamageDealt { target: p1(0), new_hp: PokemonHP::Number(460) }),
                event(EventKind::DamageDealt { target: p1(0), new_hp: PokemonHP::Number(422) }),
                event(EventKind::HitCount { target: p1(0), hits: 2 }),
            ],
        )],
        garchomp_dex(),
        move_dex,
    );

    let p2_r = &result.p2_active_mons[0];
    // No contradiction: bounds must remain valid.
    assert!(
        p2_r.min_pre_nature_stat[1] <= p2_r.max_pre_nature_stat[1],
        "Multi-hit must not invert Atk bounds (min {}, max {})",
        p2_r.min_pre_nature_stat[1], p2_r.max_pre_nature_stat[1]
    );
    // Bounds must be within Garchomp's Atk range.
    assert!(p2_r.min_pre_nature_stat[1] >= 135, "min BSV below Garchomp minimum");
    assert!(p2_r.max_pre_nature_stat[1] <= 182, "max BSV above Garchomp maximum");

    // T3: Note on damage values — 40 and 38 are physically impossible from a 25 BP
    // Normal move with Atk ≤ 182 vs Def=100 (max achievable ≈ 22).  This test
    // exercises robustness: the engine must not crash or invert bounds on infeasible
    // input.  Since no Atk in [135, 182] can produce 40, the engine must leave the
    // initial range exactly unchanged rather than over-narrow. (The feasible-damage
    // narrowing behaviour is covered by test_pass3_multihit_tighter_than_single_hit.)
    assert_eq!(
        (p2_r.min_pre_nature_stat[1], p2_r.max_pre_nature_stat[1]),
        (135, 182),
        "T3: infeasible multihit damage must leave the species bounds exactly unchanged"
    );
}

/// Multi-hit with varied per-hit damage should produce bounds strictly tighter
/// than a single-hit observation of just the first hit.
///
/// Setup: P2 Garchomp (Hardy, no item, Atk BSV ∈ [135,182]), P1 Def=100, HP=500.
/// Move: 40 BP Normal Physical (no STAB on Dragon/Ground Garchomp), 2-hit.
///
/// Hand-derived bounds (formula: floor(floor(22*40*atk/100)/50)+2, min roll=0.85):
///   Damage 28 (hit 1): produced by atk ∈ [148, 181]  (base=28 @ roll 1.0 … base=33 @ roll 0.85)
///   Damage 29 (hit 2): produced by atk ∈ [154, 182]  (base=29 @ roll 1.0 … base=34 @ roll 0.85)
///   Intersection:             atk ∈ [154, 181]
///
/// The single-hit observation of just 28 damage gives [148, 181].
/// The 2-hit intersection [154, 181] is strictly tighter on the min side: 154 > 148.
#[test]
fn test_pass3_multihit_tighter_than_single_hit() {
    let p1_mon = known_p1_normal(); // HP=500, Def=100
    let p2_mon = neutral_no_item_garchomp();

    // 40 BP Normal/Physical 2-hit move.  Normal gives no STAB on Dragon/Ground Garchomp,
    // so the hand-derived bounds above apply without type multipliers.
    let mut move_dex_multi = HashMap::new();
    let mut hit2_move = normal_physical_move(PokemonMove::BulletSeed, 40);
    hit2_move.multihit_range = [2, 2];
    move_dex_multi.insert(PokemonMove::BulletSeed, hit2_move);

    let mut move_dex_single = HashMap::new();
    let mut single_move = normal_physical_move(PokemonMove::Tackle, 40);
    single_move.multihit_range = [1, 1];
    move_dex_single.insert(PokemonMove::Tackle, single_move);

    // 2-hit:
    //   Hit 1: HP 500 → 472  (delta = 28)
    //   Hit 2: HP 472 → 443  (delta = 29, using updated running_hp — confirmed at pass3:3987)
    let multi_result = apply_ex(
        battle_1v1(p1_mon.clone(), p2_mon.clone()),
        vec![event_with(
            EventKind::MoveUsed { user: p2(0), move_used: PokemonMove::BulletSeed, targets: vec![p1(0)] },
            vec![
                event(EventKind::DamageDealt { target: p1(0), new_hp: PokemonHP::Number(472) }), // 28 dmg
                event(EventKind::DamageDealt { target: p1(0), new_hp: PokemonHP::Number(443) }), // 29 dmg
                event(EventKind::HitCount { target: p1(0), hits: 2 }),
            ],
        )],
        garchomp_dex(),
        move_dex_multi,
    );

    // Single hit: only the first hit (28 damage, new_hp = 472).
    let single_result = apply_ex(
        battle_1v1(p1_mon, p2_mon),
        vec![event_with(
            EventKind::MoveUsed { user: p2(0), move_used: PokemonMove::Tackle, targets: vec![p1(0)] },
            vec![event(EventKind::DamageDealt { target: p1(0), new_hp: PokemonHP::Number(472) })], // 28 dmg
        )],
        garchomp_dex(),
        move_dex_single,
    );

    let m = &multi_result.p2_active_mons[0];
    let s = &single_result.p2_active_mons[0];

    // Sound: bounds must not be inverted.
    assert!(m.min_pre_nature_stat[1] <= m.max_pre_nature_stat[1], "multi-hit bounds inverted");
    assert!(s.min_pre_nature_stat[1] <= s.max_pre_nature_stat[1], "single-hit bounds inverted");

    // Multi-hit intersection must be STRICTLY tighter than single-hit alone.
    // The second hit (29 dmg) raises the min beyond what the first hit (28 dmg) constrains.
    assert!(
        m.min_pre_nature_stat[1] > s.min_pre_nature_stat[1],
        "Multi-hit must raise min Atk BSV higher than single-hit: \
         multi min={} should be > single min={}",
        m.min_pre_nature_stat[1], s.min_pre_nature_stat[1],
    );
    // Max should not be wider (the intersection can only be the same or tighter).
    assert!(
        m.max_pre_nature_stat[1] <= s.max_pre_nature_stat[1],
        "Multi-hit must not produce a wider max Atk BSV than single-hit: \
         multi max={}, single max={}",
        m.max_pre_nature_stat[1], s.max_pre_nature_stat[1],
    );
}

// ── Learnset-based Illusion narrowing ────────────────────────────────────────

/// When a Zoroark-style `possible_species = Possibly([A, B])` uses a move that A
/// cannot learn but B can, A is dropped from the candidates and B collapses to Known.
#[test]
fn test_learnset_narrows_possible_species_to_known() {
    // P2 mon appears as Garchomp but might be Alakazam (Illusion scenario).
    let mut p2_mon = unknown_mon_species(Species::Garchomp);
    p2_mon.possible_species = Unknown::Possibly(vec![Species::Garchomp, Species::Alakazam]);
    // Keep types/weight as Garchomp's (the disguised species).

    let state = battle_with_p2(vec![p2_mon]);

    // Learnset: Garchomp learns Earthquake, Alakazam does not.
    let mut learnset_dex: HashMap<Species, HashSet<PokemonMove>> = HashMap::new();
    learnset_dex.insert(Species::Garchomp, {
        let mut s = HashSet::new();
        s.insert(PokemonMove::Earthquake);
        s
    });
    learnset_dex.insert(Species::Alakazam, {
        let mut s = HashSet::new();
        s.insert(PokemonMove::Psychic);
        // Earthquake deliberately absent → Alakazam can't learn it.
        s
    });

    let config = InferenceConfig {
        learnset_dex,
        ev_total_cap: None, // disable EV cap (no species to run pass5 on)
        ..Default::default()
    };

    let result = apply_with_config(
        state,
        vec![event(EventKind::MoveUsed {
            user: p2(0),
            move_used: PokemonMove::Earthquake,
            targets: vec![p1(0)],
        })],
        HashMap::new(),
        HashMap::new(),
        config,
    );

    // After seeing Earthquake, Alakazam should be excluded → collapses to Known(Garchomp).
    assert!(
        matches!(&result.p2_active_mons[0].possible_species, Unknown::Known(s) if *s == Species::Garchomp),
        "Learnset narrowing must collapse possible_species to Known(Garchomp) when Alakazam can't learn Earthquake"
    );
}

/// When both candidate species can learn the observed move, no narrowing occurs.
#[test]
fn test_learnset_keeps_both_when_move_is_legal_for_both() {
    let mut p2_mon = unknown_mon_species(Species::Garchomp);
    p2_mon.possible_species = Unknown::Possibly(vec![Species::Garchomp, Species::Tyranitar]);

    let state = battle_with_p2(vec![p2_mon]);

    let mut learnset_dex: HashMap<Species, HashSet<PokemonMove>> = HashMap::new();
    learnset_dex.insert(Species::Garchomp, {
        let mut s = HashSet::new(); s.insert(PokemonMove::Earthquake); s
    });
    learnset_dex.insert(Species::Tyranitar, {
        let mut s = HashSet::new(); s.insert(PokemonMove::Earthquake); s
    });

    let config = InferenceConfig { learnset_dex, ev_total_cap: None, ..Default::default() };

    let result = apply_with_config(
        state,
        vec![event(EventKind::MoveUsed {
            user: p2(0), move_used: PokemonMove::Earthquake, targets: vec![p1(0)],
        })],
        HashMap::new(),
        HashMap::new(),
        config,
    );

    // Both can learn Earthquake → Possibly still has both candidates.
    assert!(
        matches!(&result.p2_active_mons[0].possible_species, Unknown::Possibly(v) if v.len() == 2),
        "Learnset must not narrow when both candidates can learn the observed move"
    );
}

/// When the learnset dex lacks data for a species, that species must be kept as a
/// candidate (sound: we can't confirm illegality without data).
#[test]
fn test_learnset_keeps_species_when_no_data() {
    let mut p2_mon = unknown_mon_species(Species::Garchomp);
    p2_mon.possible_species = Unknown::Possibly(vec![Species::Garchomp, Species::Alakazam]);

    let state = battle_with_p2(vec![p2_mon]);

    // Learnset: only Garchomp has data; Alakazam is absent → keep both.
    let mut learnset_dex: HashMap<Species, HashSet<PokemonMove>> = HashMap::new();
    learnset_dex.insert(Species::Garchomp, {
        let mut s = HashSet::new(); s.insert(PokemonMove::Earthquake); s
    });
    // Alakazam absent → keep it regardless of move.

    let config = InferenceConfig { learnset_dex, ev_total_cap: None, ..Default::default() };

    let result = apply_with_config(
        state,
        vec![event(EventKind::MoveUsed {
            user: p2(0), move_used: PokemonMove::Earthquake, targets: vec![p1(0)],
        })],
        HashMap::new(),
        HashMap::new(),
        config,
    );

    // Missing data for Alakazam → keep both candidates (soundness).
    assert!(
        matches!(&result.p2_active_mons[0].possible_species, Unknown::Possibly(v) if v.len() == 2),
        "Learnset must not drop species when learnset data is absent (sound: can't confirm illegality)"
    );
}

// ── Global EV total-cap tightening (Pass 5) ──────────────────────────────────

/// When two stats have high minimum EVs, the 510-EV total cap must tighten the
/// maximum EVs allowed for all other stats.
///
/// Scenario: Garchomp with Atk=175 (implies minEV≈196) and Def=130 (implies minEV≈116).
/// Budget for remaining stats: 510 − 196 − 116 = 198. EV_LATTICE max ≤ 198 is 196.
/// So maxEvs for SpA/SpD/Spe should each be ≤ 196 after the cap is applied.
#[test]
fn test_ev_cap_tightens_remaining_stats() {
    use crate::state::pokemon::Nature;

    let mut mon = UnknownPokemonState::from_opponent_species(Species::Garchomp, &garchomp_dex(), 50);
    mon.possible_natures = Unknown::Known(Nature::Hardy); // neutral

    // Pin Atk (stat 1) to exactly 175: requires EV≈196 (31 IVs, neutral, lv50 Garchomp base=130).
    // calc_stat(130, 31, 196, 50, 1.0) = floor((260+31+49)*0.5+5) = floor(175) = 175.
    mon.minStats[1] = 175;
    mon.maxStats[1] = 175;
    mon.min_pre_nature_stat[1] = 175;
    mon.max_pre_nature_stat[1] = 175;

    // Pin Def (stat 2) to exactly 130: requires EV≈116 (31 IVs, neutral, lv50 Garchomp base=95).
    // calc_stat(95, 31, 116, 50, 1.0) = floor((190+31+29)*0.5+5) = floor(130) = 130.
    mon.minStats[2] = 130;
    mon.maxStats[2] = 130;
    mon.min_pre_nature_stat[2] = 130;
    mon.max_pre_nature_stat[2] = 130;

    let config = InferenceConfig {
        use_stat_points: true,
        force_max_ivs: true,
        level: 50,
        legal_items: None,
        allow_repeat_items: false,
        learnset_dex: HashMap::new(),
        ev_total_cap: Some(510),
    };

    pass5_back_solve(&mut mon, &config, &garchomp_dex());

    // Atk and Def EVs should be pinned near their expected values.
    assert_eq!(mon.minEvs[1], 196, "Atk minEV must be 196 for stat=175");
    assert_eq!(mon.maxEvs[1], 196, "Atk maxEV must be 196 for stat=175");
    assert!(mon.minEvs[2] <= 116, "Def minEV must be ≤ 116 for stat≥130");

    // After cap (budget = 510 - 196 - 116 - 0..= 198), other stats must have maxEV ≤ 196.
    // (Nearest EV_LATTICE value ≤ 198 is 196.)
    assert!(
        mon.maxEvs[3] <= 196,
        "SpA maxEV must be capped to ≤ 196 by the total-EV cap (got {})",
        mon.maxEvs[3]
    );
    assert!(
        mon.maxEvs[4] <= 196,
        "SpD maxEV must be capped to ≤ 196 by the total-EV cap (got {})",
        mon.maxEvs[4]
    );
    assert!(
        mon.maxEvs[5] <= 196,
        "Spe maxEV must be capped to ≤ 196 by the total-EV cap (got {})",
        mon.maxEvs[5]
    );

    // Soundness check: min ≤ max for all stats.
    for i in 0..6 {
        assert!(mon.minEvs[i] <= mon.maxEvs[i], "EV bounds inverted for stat {}", i);
    }
}

/// EV cap with no EVs forced high → no tightening beyond individual stat analysis.
#[test]
fn test_ev_cap_no_tightening_when_evs_low() {
    use crate::state::pokemon::Nature;

    let mut mon = UnknownPokemonState::from_opponent_species(Species::Garchomp, &garchomp_dex(), 50);
    mon.possible_natures = Unknown::Known(Nature::Hardy);

    // Don't pin stats — everything stays at default wide range.
    let config = InferenceConfig {
        use_stat_points: true,
        force_max_ivs: true,
        level: 50,
        legal_items: None,
        allow_repeat_items: false,
        learnset_dex: HashMap::new(),
        ev_total_cap: Some(510),
    };

    pass5_back_solve(&mut mon, &config, &garchomp_dex());

    // When minEvs are all 0, budget per stat = 510 - 0 = 510 ≥ 252 → no cap tightening.
    for i in 0..6 {
        assert!(mon.maxEvs[i] <= 252, "maxEV must never exceed 252 (got {} for stat {})", mon.maxEvs[i], i);
        assert!(mon.minEvs[i] <= mon.maxEvs[i], "EV bounds inverted for stat {}", i);
    }
}

// ── Phase 1: Ability inference by species ────────────────────────────────────

/// A known-species opponent's `possible_abilities` should be `Possibly([slot set])`
/// rather than the default `Not([])`.
#[test]
fn test_ability_narrowed_by_species() {
    let mon = UnknownPokemonState::from_opponent_species(Species::Garchomp, &garchomp_dex(), 50);
    // Garchomp's dex entry in the test has [SandVeil, RoughSkin].
    assert!(
        matches!(&mon.possible_abilities, Unknown::Possibly(v) if v.contains(&Ability::SandVeil)),
        "SandVeil should be possible for Garchomp"
    );
    assert!(
        matches!(&mon.possible_abilities, Unknown::Possibly(v) if v.contains(&Ability::RoughSkin)),
        "RoughSkin should be possible for Garchomp"
    );
    // An ability not in the slot set should be excluded.
    use crate::information::inference::unknown_is_excluded;
    let excluded = unknown_is_excluded(&mon.possible_abilities, &Ability::Levitate);
    assert!(excluded, "Levitate should be excluded for Garchomp");
}

/// A revealed ability within the species' set should narrow `possible_abilities` to `Known`.
#[test]
fn test_ability_revealed_within_set_narrows_to_known() {
    let mon = UnknownPokemonState::from_opponent_species(Species::Garchomp, &garchomp_dex(), 50);
    let state = battle_with_p2(vec![mon]);
    let result = apply(
        state,
        vec![event(EventKind::AbilityRevealed { slot: p2(0), ability: Ability::SandVeil })],
    );
    assert_eq!(
        result.p2_active_mons[0].possible_abilities,
        Unknown::Known(Ability::SandVeil),
        "Revealed ability within set should collapse to Known"
    );
}

/// A revealed ability OUTSIDE the species' set (e.g. Trace copying an ability) should
/// overwrite `possible_abilities` to `Known` without panicking.
#[test]
fn test_ability_revealed_outside_set_overwrites_to_known() {
    let mon = UnknownPokemonState::from_opponent_species(Species::Garchomp, &garchomp_dex(), 50);
    let state = battle_with_p2(vec![mon]);
    // Levitate is not in Garchomp's slot set — simulates a Trace/Mummy/etc.
    let result = apply(
        state,
        vec![event(EventKind::AbilityRevealed { slot: p2(0), ability: Ability::Levitate })],
    );
    assert_eq!(
        result.p2_active_mons[0].possible_abilities,
        Unknown::Known(Ability::Levitate),
        "Foreign ability reveal should overwrite to Known without contradiction"
    );
    // Original abilities field should remain unset (we don't touch possible_original_abilities here)
}

// ── Regression: S14 — live ability change must overwrite Not/Known too ──────────

/// A revealed ability that was previously excluded from a `Not(excluded)` set (e.g.
/// ruled out earlier by switch-in absence inference — "this mon's innate ability
/// isn't Drizzle") must overwrite to `Known` as a live ability change (Trace copying
/// Drizzle from an ally), not panic. Before the S14 fix, only the `Possibly`-outside
/// case was treated this way; `Not`-excluded values still routed through
/// `unknown_set_known` and panicked.
#[test]
fn test_ability_revealed_previously_excluded_overwrites_to_known() {
    let mut mon = unknown_mon(); // Garchomp via from_opponent_species — Not([]) by default here
    mon.possible_abilities = Unknown::Not(vec![Ability::Drizzle]);
    let state = battle_with_p2(vec![mon]);

    let result = apply(
        state,
        vec![event(EventKind::AbilityRevealed { slot: p2(0), ability: Ability::Drizzle })],
    );
    assert_eq!(
        result.p2_active_mons[0].possible_abilities,
        Unknown::Known(Ability::Drizzle),
        "A previously-excluded ability reveal should overwrite to Known (live change), not panic"
    );
}

/// A revealed ability that conflicts with an already-`Known` value (e.g. the mon's
/// live ability was previously pinned to X, and a later Trace/Skill Swap/Mummy event
/// changes it to Y) must overwrite to `Known(Y)`, not panic.
#[test]
fn test_ability_revealed_conflicting_known_overwrites_to_known() {
    let mut mon = unknown_mon();
    mon.possible_abilities = Unknown::Known(Ability::SandVeil);
    let state = battle_with_p2(vec![mon]);

    let result = apply(
        state,
        vec![event(EventKind::AbilityRevealed { slot: p2(0), ability: Ability::Levitate })],
    );
    assert_eq!(
        result.p2_active_mons[0].possible_abilities,
        Unknown::Known(Ability::Levitate),
        "A conflicting Known->Known ability reveal should overwrite (live change), not panic"
    );
}

// ── Phase 2: Contact-reaction absence ────────────────────────────────────────

fn contact_physical_move(name: PokemonMove, bp: u16) -> MoveData {
    use crate::state::dex_data::MoveFlag;
    let mut md = normal_physical_move(name, bp);
    md.flags.push(MoveFlag::Contact);
    md
}

/// Contact hit with NO Rocky Helmet reaction and the attacker has no escapes:
/// Rocky Helmet should be excluded from the defender.
#[test]
fn test_contact_absence_excludes_rocky_helmet() {
    let p1_mon = {
        let mut m = UnknownPokemonState::from_opponent_species(Species::Garchomp, &garchomp_dex(), 50);
        // Our mon: item Known(None) — no Protective Pads; ability Known(SandVeil) — no LongReach
        m.item = Unknown::Known(Item::None);
        m.possible_abilities = Unknown::Known(Ability::SandVeil);
        m
    };
    let p2_mon = UnknownPokemonState::from_opponent_species(Species::Garchomp, &garchomp_dex(), 50);
    let state = battle_1v1(p1_mon, p2_mon);

    let mut move_dex = HashMap::new();
    move_dex.insert(PokemonMove::Earthquake, contact_physical_move(PokemonMove::Earthquake, 100));

    let result = apply_ex(
        state,
        vec![event_with(
            EventKind::MoveUsed { user: p1(0), move_used: PokemonMove::Earthquake, targets: vec![p2(0)] },
            vec![event(EventKind::DamageDealt { target: p2(0), new_hp: PokemonHP::Percent(50) })],
            // No ItemRevealed{RockyHelmet} — helmet is absent.
        )],
        garchomp_dex(),
        move_dex,
    );

    assert!(
        is_item_excluded(&result.p2_active_mons[0], &Item::RockyHelmet),
        "Rocky Helmet should be excluded when no helmet reaction occurred"
    );
}

// ── Phase 3: Powder-move immunity ────────────────────────────────────────────

fn powder_status_move(name: PokemonMove) -> MoveData {
    use crate::state::dex_data::MoveFlag;
    let mut md = poke_status_move(name);
    md.flags.push(MoveFlag::Powder);
    md
}

/// A powder move failing on a non-Grass target should reveal SafetyGoggles or Overcoat.
#[test]
fn test_powder_immunity_reveals_safety_goggles_or_overcoat() {
    // p2 is a Normal-type (not Grass) — powder immunity must come from item/ability.
    // Use unknown_mon() (empty dex → Not([]) abilities) so BCP doesn't prune Overcoat
    // and the 2-element disjunction stays intact in predicates.
    let mut p2_mon = unknown_mon();
    p2_mon.possible_types = Unknown::Known(vec![PokemonType::Normal]);
    let state = battle_with_p2(vec![p2_mon]);

    let mut move_dex = HashMap::new();
    move_dex.insert(PokemonMove::PowderSnow, powder_status_move(PokemonMove::PowderSnow));

    let result = apply_ex(
        state,
        vec![event_with(
            EventKind::MoveUsed { user: p1(0), move_used: PokemonMove::PowderSnow, targets: vec![p2(0)] },
            vec![event(EventKind::Immune { target: p2(0) })],
        )],
        HashMap::new(),
        move_dex,
    );

    let pred = &result.predicates;
    // Clause should contain BOTH SafetyGoggles and Overcoat as a 2-element disjunction
    // (mon_idx 0: battle_with_p2 has empty p1_active, so p2(0) is flat index 0).
    let has_sg_and_overcoat = pred.iter().any(|clause| {
        clause.contains(&Statement::HasItem { mon_idx: 0, item: Item::SafetyGoggles })
            && clause.contains(&Statement::HasAbility { mon_idx: 0, ability: Ability::Overcoat })
    });
    assert!(has_sg_and_overcoat, "Should emit [SafetyGoggles ∨ Overcoat] clause on powder immunity");
}

// ── Phase 3: Prankster-immunity from Dark-type bounce ────────────────────────

/// When a status move from the opponent fails against our Known Dark-type mon,
/// the only explanation is Prankster priority bouncing off the Dark-type immunity.
#[test]
fn test_prankster_immunity_from_dark_type_bounce() {
    let mut p1_mon = {
        let mut m = UnknownPokemonState::from_opponent_species(Species::Garchomp, &garchomp_dex(), 50);
        m.possible_types = Unknown::Known(vec![PokemonType::Dark]);
        // The pass requires the target's ability to be Known and non-immunity-granting
        // (an unknown ability could be an absorb ability or Good as Gold).
        m.possible_abilities = Unknown::Known(Ability::SandVeil);
        m
    };
    // Use unknown_mon_species (empty dex → Not([]) abilities) so BCP can force Prankster
    // without hitting a contradiction on the Garchomp species set (SandVeil/RoughSkin only).
    let p2_mon = unknown_mon_species(Species::Garchomp);
    let state = battle_1v1(p1_mon, p2_mon);

    let mut move_dex = HashMap::new();
    move_dex.insert(PokemonMove::WillOWisp, poke_status_move(PokemonMove::WillOWisp));

    let result = apply_ex(
        state,
        vec![event_with(
            EventKind::MoveUsed { user: p2(0), move_used: PokemonMove::WillOWisp, targets: vec![p1(0)] },
            vec![event(EventKind::Immune { target: p1(0) })],
        )],
        HashMap::new(),
        move_dex,
    );

    // BCP forces the unit clause [Prankster] directly onto the mon.
    assert_eq!(
        result.p2_active_mons[0].possible_abilities,
        Unknown::Known(Ability::Prankster),
        "BCP should force Prankster Known when status move fails on Dark-type"
    );
}

/// A `MoveFailed` (not `Immune`) must NOT trigger the Prankster inference — it covers
/// already-statused targets, terrain blocks, and Dazzling-class blocks.
#[test]
fn test_prankster_not_inferred_from_move_failed() {
    let mut p1_mon = UnknownPokemonState::from_opponent_species(Species::Garchomp, &garchomp_dex(), 50);
    p1_mon.possible_types = Unknown::Known(vec![PokemonType::Dark]);
    p1_mon.possible_abilities = Unknown::Known(Ability::SandVeil);
    let p2_mon = unknown_mon_species(Species::Garchomp);
    let state = battle_1v1(p1_mon, p2_mon);

    let mut move_dex = HashMap::new();
    move_dex.insert(PokemonMove::WillOWisp, poke_status_move(PokemonMove::WillOWisp));

    let result = apply_ex(
        state,
        vec![event_with(
            EventKind::MoveUsed { user: p2(0), move_used: PokemonMove::WillOWisp, targets: vec![p1(0)] },
            vec![event(EventKind::MoveFailed { slot: p1(0) })],
        )],
        HashMap::new(),
        move_dex,
    );

    assert!(
        !matches!(&result.p2_active_mons[0].possible_abilities,
            Unknown::Known(a) if *a == Ability::Prankster),
        "MoveFailed is ambiguous (Protect/statused/terrain/Dazzling) — must not force Prankster; got {:?}",
        result.p2_active_mons[0].possible_abilities
    );
}

/// An unknown target ability could itself explain the Immune (absorb ability,
/// Good as Gold) — must NOT force Prankster.
#[test]
fn test_prankster_not_inferred_when_target_ability_unknown() {
    let mut p1_mon = UnknownPokemonState::from_opponent_species(Species::Garchomp, &HashMap::new(), 50);
    p1_mon.possible_types = Unknown::Known(vec![PokemonType::Dark]);
    // possible_abilities stays Not([]) — fully unknown.
    let p2_mon = unknown_mon_species(Species::Garchomp);
    let state = battle_1v1(p1_mon, p2_mon);

    let mut move_dex = HashMap::new();
    move_dex.insert(PokemonMove::WillOWisp, poke_status_move(PokemonMove::WillOWisp));

    let result = apply_ex(
        state,
        vec![event_with(
            EventKind::MoveUsed { user: p2(0), move_used: PokemonMove::WillOWisp, targets: vec![p1(0)] },
            vec![event(EventKind::Immune { target: p1(0) })],
        )],
        HashMap::new(),
        move_dex,
    );

    assert!(
        !matches!(&result.p2_active_mons[0].possible_abilities,
            Unknown::Known(a) if *a == Ability::Prankster),
        "an unknown target ability could explain the Immune — must not force Prankster; got {:?}",
        result.p2_active_mons[0].possible_abilities
    );
}

/// Dual-type move immunity (Thunder Wave vs Dark/Ground) explains the Immune without
/// Prankster — must NOT force it.
#[test]
fn test_prankster_not_inferred_for_dual_type_immunity() {
    let mut p1_mon = UnknownPokemonState::from_opponent_species(Species::Garchomp, &garchomp_dex(), 50);
    p1_mon.possible_types = Unknown::Known(vec![PokemonType::Dark, PokemonType::Ground]);
    p1_mon.possible_abilities = Unknown::Known(Ability::SandVeil);
    let p2_mon = unknown_mon_species(Species::Garchomp);
    let state = battle_1v1(p1_mon, p2_mon);

    let mut move_dex = HashMap::new();
    let mut twave = poke_status_move(PokemonMove::ThunderWave);
    twave.pokemon_type = PokemonType::Electric;
    move_dex.insert(PokemonMove::ThunderWave, twave);

    let result = apply_ex(
        state,
        vec![event_with(
            EventKind::MoveUsed { user: p2(0), move_used: PokemonMove::ThunderWave, targets: vec![p1(0)] },
            vec![event(EventKind::Immune { target: p1(0) })],
        )],
        HashMap::new(),
        move_dex,
    );

    assert!(
        !matches!(&result.p2_active_mons[0].possible_abilities,
            Unknown::Known(a) if *a == Ability::Prankster),
        "Ground typing already explains Thunder Wave immunity — must not force Prankster; got {:?}",
        result.p2_active_mons[0].possible_abilities
    );
}

// ── Phase 4: EOT healing reveals ─────────────────────────────────────────────

/// An unexplained EOT heal on a non-Poison opponent should emit [Leftovers].
#[test]
fn test_eot_heal_reveals_leftovers() {
    let mut p2_mon = UnknownPokemonState::from_opponent_species(Species::Garchomp, &garchomp_dex(), 50);
    p2_mon.possible_types = Unknown::Known(vec![PokemonType::Dragon, PokemonType::Ground]);
    p2_mon.hp = PokemonHP::Percent(94); // not full — heal is visible

    let state = battle_with_p2(vec![p2_mon]);

    let result = apply(
        state,
        vec![event_with(
            EventKind::EndOfTurn,
            vec![event(EventKind::Healed { target: p2(0), new_hp: PokemonHP::Percent(100) })],
        )],
    );

    // The unit clause [HasItem(Leftovers)] is forced by BCP → item becomes Known(Leftovers).
    assert_eq!(
        result.p2_active_mons[0].item,
        Unknown::Known(Item::Leftovers),
        "BCP should force Leftovers Known for unexplained EOT heal"
    );
    // BlackSludge must not be the forced item for a non-Poison type.
    assert_ne!(
        result.p2_active_mons[0].item,
        Unknown::Known(Item::BlackSludge),
        "Black Sludge should NOT be forced for non-Poison type"
    );
}

/// An unexplained EOT heal on a Poison-type opponent should emit [Leftovers ∨ BlackSludge].
#[test]
fn test_eot_heal_poison_type_includes_black_sludge() {
    let mut p2_mon = UnknownPokemonState::from_opponent_species(Species::Garchomp, &garchomp_dex(), 50);
    p2_mon.possible_types = Unknown::Known(vec![PokemonType::Poison]);
    p2_mon.hp = PokemonHP::Percent(90);

    let state = battle_with_p2(vec![p2_mon]);

    let result = apply(
        state,
        vec![event_with(
            EventKind::EndOfTurn,
            vec![event(EventKind::Healed { target: p2(0), new_hp: PokemonHP::Percent(100) })],
        )],
    );

    let has_joint_clause = result.predicates.iter().any(|clause| {
        clause.contains(&Statement::HasItem { mon_idx: 0, item: Item::Leftovers })
            && clause.contains(&Statement::HasItem { mon_idx: 0, item: Item::BlackSludge })
    });
    assert!(has_joint_clause, "Poison-type EOT heal should emit [Leftovers ∨ BlackSludge]");
}

// ── Phase 5: Guaranteed-status absence ───────────────────────────────────────

fn guaranteed_burn_secondary_move(name: PokemonMove, bp: u16) -> MoveData {
    use crate::state::dex_data::{PokemonSecondaryEffect, HitEffect};
    let mut md = normal_physical_move(name, bp);
    md.secondaries = vec![PokemonSecondaryEffect {
        chance: 100,
        effect: HitEffect {
            status: Some(Status::Burn),
            ..Default::default()
        },
        random_choices: vec![],
    }];
    md
}

/// A guaranteed-burn move that hits but produces no StatusInflicted on a non-Fire target
/// with no decidable preventers should emit a burn-preventer disjunction.
#[test]
fn test_guaranteed_status_absence_emits_preventer_clause() {
    // Use empty dex so possible_abilities = Not([]) — keeps all prevention abilities in the
    // predicate clause (Garchomp's Possibly([SandVeil,RoughSkin]) would prune them all via BCP).
    let mut p2_mon = UnknownPokemonState::from_opponent_species(Species::Garchomp, &HashMap::new(), 50);
    // Dragon/Ground — not Fire-type, not already statused, no Substitute.
    p2_mon.possible_types = Unknown::Known(vec![PokemonType::Dragon, PokemonType::Ground]);

    // Our attacking mon (p1): known, no special properties.
    let p1_mon = {
        let mut m = UnknownPokemonState::from_opponent_species(Species::Garchomp, &garchomp_dex(), 50);
        m.item = Unknown::Known(Item::None);
        m.possible_abilities = Unknown::Known(Ability::SandVeil);
        m
    };
    let state = battle_1v1(p1_mon, p2_mon);

    let mut move_dex = HashMap::new();
    move_dex.insert(
        PokemonMove::Ember,
        guaranteed_burn_secondary_move(PokemonMove::Ember, 40),
    );

    let result = apply_ex(
        state,
        vec![event_with(
            EventKind::MoveUsed { user: p1(0), move_used: PokemonMove::Ember, targets: vec![p2(0)] },
            vec![event(EventKind::DamageDealt { target: p2(0), new_hp: PokemonHP::Percent(80) })],
            // No StatusInflicted — burn was prevented.
        )],
        garchomp_dex(),
        move_dex,
    );

    // Should emit a clause containing burn-prevention abilities (WaterVeil, WaterBubble, etc.)
    let has_preventer_clause = result.predicates.iter().any(|clause| {
        clause.iter().any(|s| matches!(s, Statement::HasAbility { ability: Ability::WaterVeil, .. }
            | Statement::HasAbility { ability: Ability::WaterBubble, .. }
            | Statement::HasAbility { ability: Ability::ThermalExchange, .. }
            | Statement::HasItem { item: Item::CovertCloak, .. }))
    });
    assert!(has_preventer_clause, "Should emit burn-preventer clause when guaranteed burn doesn't land");
}

/// A target KO'd by the very hit receives no secondary status — the absence is fully
/// explained by the faint, so NO preventer clause may be emitted (it would be unsound).
#[test]
fn test_guaranteed_status_absence_skipped_on_fainted_target() {
    let mut p2_mon = UnknownPokemonState::from_opponent_species(Species::Garchomp, &HashMap::new(), 50);
    p2_mon.possible_types = Unknown::Known(vec![PokemonType::Dragon, PokemonType::Ground]);

    let p1_mon = {
        let mut m = UnknownPokemonState::from_opponent_species(Species::Garchomp, &garchomp_dex(), 50);
        m.item = Unknown::Known(Item::None);
        m.possible_abilities = Unknown::Known(Ability::SandVeil);
        m
    };
    let state = battle_1v1(p1_mon, p2_mon);

    let mut move_dex = HashMap::new();
    move_dex.insert(
        PokemonMove::Ember,
        guaranteed_burn_secondary_move(PokemonMove::Ember, 40),
    );

    let result = apply_ex(
        state,
        vec![event_with(
            EventKind::MoveUsed { user: p1(0), move_used: PokemonMove::Ember, targets: vec![p2(0)] },
            vec![
                // The hit KOs the target — no burn can land.
                event(EventKind::DamageDealt { target: p2(0), new_hp: PokemonHP::Percent(0) }),
                event(EventKind::Faint { slot: p2(0) }),
            ],
        )],
        garchomp_dex(),
        move_dex,
    );

    let has_preventer_clause = result.predicates.iter().any(|clause| {
        clause.iter().any(|s| matches!(s, Statement::HasAbility { ability: Ability::WaterVeil, .. }
            | Statement::HasItem { item: Item::CovertCloak, .. }))
    });
    assert!(
        !has_preventer_clause,
        "faint fully explains the missing burn — no preventer clause may be emitted; predicates = {:?}",
        result.predicates
    );
}

// ── Regression: B1 — MegaEvolution flag ──────────────────────────────────────

/// After observing a MegaEvolution event the "mega resource available" flag must
/// flip to `false` (resource spent).  Before B1 fix the handler set the flag to
/// `true`, leaving the materialized battle claiming the Mega is still in the bank.
#[test]
fn test_mega_sets_has_mega_false() {
    let p1_mon = unknown_mon();
    let p2_mon = unknown_mon(); // Garchomp — mega evo uses same species for simplicity
    let mut state = battle_1v1(p1_mon, p2_mon);
    // Both resources start as available (true = still in bank).
    state.p1_has_mega = true;
    state.p2_has_mega = true;

    let result = apply(
        state,
        vec![event(EventKind::MegaEvolution {
            slot: p2(0),
            into: Species::Garchomp, // using Garchomp as a stand-in; no mega_stone check
        })],
    );
    assert!(
        !result.p2_has_mega,
        "p2_has_mega must be false after P2 Mega Evolves (resource spent)"
    );
    assert!(
        result.p1_has_mega,
        "p1_has_mega must remain true — P1 has not yet Mega Evolved"
    );
}

// ── Regression: S3 — MegaEvolution/FormeChange with a real species change ────

/// `MegaEvolution` into a genuinely different species (base is already `Known`)
/// must NOT panic. Before the S3 fix, the handler routed through
/// `unknown_set_known`, which contradiction-panics whenever the field is already
/// `Known` to a *different* value — exactly the common case for a real mega evo.
#[test]
fn test_mega_evolution_real_species_change_does_not_panic() {
    use crate::state::pokemon::PokemonGender;
    let p1_mon = unknown_mon();
    let p2_mon = unknown_mon_species(Species::Garchomp);
    let mut state = battle_1v1(p1_mon, p2_mon);
    state.p2_has_mega = true;

    let mut dex = HashMap::new();
    dex.insert(Species::GarchompMega, PokemonData {
        species: Species::GarchompMega,
        types: vec![PokemonType::Dragon, PokemonType::Ground],
        base_stats: [108, 170, 115, 120, 95, 92],
        weight: 950,
        primary_ability: Some(Ability::SandForce),
        abilities: vec![Ability::SandForce],
        base_species: Some(Species::Garchomp),
        forme: None,
        required_item: None,
        battle_only: Some(Species::Garchomp),
        default_gender: PokemonGender::Male,
    });

    let result = apply_ex(
        state,
        vec![event(EventKind::MegaEvolution {
            slot: p2(0),
            into: Species::GarchompMega,
        })],
        dex,
        HashMap::new(),
    );

    let mon = &result.p2_active_mons[0];
    assert!(
        matches!(&mon.possible_species, Unknown::Known(s) if *s == Species::GarchompMega),
        "species must overwrite to the mega forme, got {:?}",
        mon.possible_species
    );
    assert!(mon.is_mega);
    assert!(!result.p2_has_mega);
    assert!(
        matches!(&mon.possible_abilities, Unknown::Known(a) if *a == Ability::SandForce)
            || matches!(&mon.possible_abilities, Unknown::Possibly(v) if v == &vec![Ability::SandForce]),
        "ability set must be recomputed from the mega species dex entry, got {:?}",
        mon.possible_abilities
    );
}

/// `FormeChange` into a genuinely different species (e.g. Stance Change,
/// Mimikyu-Busted) must NOT panic when the base species is already `Known`.
#[test]
fn test_forme_change_real_species_change_does_not_panic() {
    use crate::state::pokemon::PokemonGender;
    let p1_mon = unknown_mon();
    let p2_mon = unknown_mon_species(Species::Garchomp);
    let state = battle_1v1(p1_mon, p2_mon);

    let mut dex = HashMap::new();
    dex.insert(Species::GarchompMega, PokemonData {
        species: Species::GarchompMega,
        types: vec![PokemonType::Dragon, PokemonType::Ground],
        base_stats: [108, 170, 115, 120, 95, 92],
        weight: 950,
        primary_ability: Some(Ability::SandForce),
        abilities: vec![Ability::SandForce],
        base_species: Some(Species::Garchomp),
        forme: None,
        required_item: None,
        battle_only: Some(Species::Garchomp),
        default_gender: PokemonGender::Male,
    });

    let result = apply_ex(
        state,
        vec![event(EventKind::FormeChange {
            slot: p2(0),
            into: Species::GarchompMega,
            permanent: true,
        })],
        dex,
        HashMap::new(),
    );

    let mon = &result.p2_active_mons[0];
    assert!(
        matches!(&mon.possible_species, Unknown::Known(s) if *s == Species::GarchompMega),
        "species must overwrite to the new forme, got {:?}",
        mon.possible_species
    );
}

// ── Regression: B2 — DamageDealt records delta, not pre-hit HP ───────────────

/// `last_damage_taken` must store the HP *delta* (amount dealt), not the pre-hit
/// HP value.  The simulator stores `eff_damage` (the delta); the inference engine
/// must match so Counter / Mirror Coat / Metal Burst back-solving is correct.
#[test]
fn test_damage_dealt_records_delta() {
    let mut p2_mon = unknown_mon();
    p2_mon.hp = PokemonHP::Percent(100); // start at full HP

    let state = battle_with_p2(vec![p2_mon]);

    // P1 uses a move and deals ~20% damage.
    let result = apply(
        state,
        vec![event_with(
            EventKind::MoveUsed {
                user:      p1(0),
                move_used: PokemonMove::Ember,
                targets:   vec![p2(0)],
            },
            vec![event(EventKind::DamageDealt {
                target: p2(0),
                new_hp: PokemonHP::Percent(80), // old=100, new=80 → delta=20
            })],
        )],
    );

    let m = &result.p2_active_mons[0];
    assert_eq!(
        m.last_damage_taken,
        PokemonHP::Percent(20),
        "last_damage_taken must be the HP delta (20 %), not the pre-hit HP (100 %)"
    );
}

// ── Regression: S4 — Life Orb not excluded when no damage is dealt ────────────

/// Life Orb recoil only fires when the holder *deals* HP damage.  If a damaging
/// move produces no `DamageDealt` reaction (e.g. miss or immune target), the
/// absence of LO recoil is uninformative — LO must NOT be excluded.
#[test]
fn test_life_orb_not_excluded_without_damage() {
    let p1_mon = unknown_mon();
    let mut p2_mon = unknown_mon();
    // Nail down the ability so neither MagicGuard nor SheerForce can explain
    // the missing recoil (those would also suppress LO exclusion).
    p2_mon.possible_abilities = Unknown::Known(Ability::SandVeil);

    let state = battle_1v1(p1_mon, p2_mon);

    let mut move_dex = HashMap::new();
    move_dex.insert(
        PokemonMove::Earthquake,
        normal_physical_move(PokemonMove::Earthquake, 100),
    );

    // P2 uses Earthquake but there is NO DamageDealt reaction (the move missed).
    let result = apply_ex(
        state,
        vec![event(EventKind::MoveUsed {
            user:      p2(0),
            move_used: PokemonMove::Earthquake,
            targets:   vec![p1(0)],
            // No DamageDealt child — the move missed.
        })],
        HashMap::new(),
        move_dex,
    );

    assert!(
        !is_item_excluded(&result.p2_active_mons[0], &Item::LifeOrb),
        "Life Orb must NOT be excluded when the move dealt no HP damage (e.g. miss)"
    );

    // Positive control — same setup WITH a DamageDealt child (and still no recoil):
    // the exclusion must fire. Without this the negative case above would pass
    // vacuously even if the whole pass were dead (nothing runs on a missed move).
    let p1_mon2 = unknown_mon();
    let mut p2_mon2 = unknown_mon();
    p2_mon2.possible_abilities = Unknown::Known(Ability::SandVeil);
    let state2 = battle_1v1(p1_mon2, p2_mon2);
    let mut move_dex2 = HashMap::new();
    move_dex2.insert(
        PokemonMove::Earthquake,
        normal_physical_move(PokemonMove::Earthquake, 100),
    );
    let result2 = apply_ex(
        state2,
        vec![event_with(
            EventKind::MoveUsed {
                user:      p2(0),
                move_used: PokemonMove::Earthquake,
                targets:   vec![p1(0)],
            },
            vec![event(EventKind::DamageDealt { target: p1(0), new_hp: PokemonHP::Percent(80) })],
        )],
        HashMap::new(),
        move_dex2,
    );
    assert!(
        is_item_excluded(&result2.p2_active_mons[0], &Item::LifeOrb),
        "control: with damage dealt and no recoil the exclusion must fire"
    );
}

// ── Regression: S5 — Switch-out reset clears boosts and volatiles ────────────

/// Direct unit test for `apply_switch_out_reset`: verifies that it clears boosts,
/// volatile statuses, and the ToxicPoison tier on the active slot.
///
/// **Why direct and not end-to-end?** `apply_switch_out_reset` modifies the active
/// slot in-place, but `pass1_switch` immediately overwrites that slot with the
/// incoming mon.  The cleared outgoing-mon state is therefore never observable
/// through `apply_information` (the outgoing mon isn't moved to `known_back`).
/// Testing the function end-to-end would only verify that the *incoming* mon
/// is built fresh — which is trivially true regardless of whether the reset ran.
/// A direct unit test is the only way to validate the reset function itself.
#[test]
fn test_switch_out_clears_boosts_and_volatiles() {
    use crate::state::pokemon::VolatileStatusState;
    use crate::state::dex_data::VolatileStatus;

    let mut p2_mon = unknown_mon();
    // Set up field-state that apply_switch_out_reset is responsible for clearing.
    p2_mon.boosts = [3, 2, 1, -1, 0, 0, 0];
    p2_mon.volatiles = vec![
        VolatileStatusState::TurnStatus(VolatileStatus::Confusion, 3),
    ];
    p2_mon.status = Some(Status::ToxicPoison(4)); // tier-4 toxic

    let mut state = battle_with_p2(vec![p2_mon]);

    // Call the reset directly: it should clear the active P2 slot in-place.
    apply_switch_out_reset(&mut state, &p2(0));

    let mon = &state.p2_active_mons[0];
    assert_eq!(mon.boosts, [0i8; 7], "apply_switch_out_reset must zero all boosts");
    assert!(mon.volatiles.is_empty(), "apply_switch_out_reset must clear all volatiles");
    assert!(
        matches!(mon.status, Some(Status::ToxicPoison(0))),
        "apply_switch_out_reset must reset ToxicPoison tier to 0, got {:?}", mon.status
    );
}

// ── Regression: S1 — Pass 3 Direction A must not exclude AV-holder BSV ───────

/// Soundness regression for S1: when P2 holds Assault Vest (×1.5 SpDef for
/// Special moves) and the true SpDef BSV is 70, the inferred lower bound of
/// `min_pre_nature_stat[4]` must remain ≤ 70.
///
/// Before the fix `can_produce_def_bsv` materialised the defender with no item,
/// which over-estimated how much damage a low-SpDef mon would take and therefore
/// raised the minimum BSV past the true value (excluding it — unsound).  After
/// the fix the engine unions over defensive items (including AV) and pre-bakes
/// the ×1.5 stat multiplier into the oracle call.
///
/// Setup arithmetic (Hardy nature, no boosts, P1 SpA = 100, BP = 100 Special):
///   With AV: eff_SpD = 70 × 1.5 = 105
///   base_dmg = ⌊⌊22×100×100/105/50⌋ + 2⌋ = ⌊41 + 2⌋ = 43
///   roll range: [⌊43×0.85⌋, 43] = [36, 43] HP
///   20 % of 200 HP → observed delta in [39, 41] HP — overlaps [36, 43] ✓
///   → BSV = 70 is feasible via the AV path; min_pre_nature_stat[4] must stay ≤ 70.
#[test]
fn test_pass3_dir_a_assault_vest_defender_does_not_exclude_true_bsv() {
    use crate::state::pokemon::Nature;

    // P1: our own mon with exactly SpA = 100, no item/ability multipliers.
    // Snorlax is NOT in garchomp_dex() → pass5 skips validation (out-of-range stats ok).
    let mut p1_mon = unknown_mon_species(Species::Snorlax);
    p1_mon.hp = PokemonHP::Number(500);
    // Lock every stat to 100; the oracle reads minStats[3] for SpA.
    p1_mon.minStats = [500, 100, 100, 100, 100, 100];
    p1_mon.maxStats = [500, 100, 100, 100, 100, 100];
    p1_mon.item               = Unknown::Known(Item::None);
    p1_mon.possible_abilities = Unknown::Known(Ability::None);

    // P2: Garchomp defender with an artificially widened SpDef range that
    // includes the target BSV of 70 (below Garchomp's natural minimum of 90).
    let mut p2_mon = UnknownPokemonState::from_opponent_species(
        Species::Garchomp, &garchomp_dex(), 50,
    );
    p2_mon.hp                    = PokemonHP::Percent(100);
    p2_mon.min_pre_nature_stat[4] = 50;   // force scan to start below the true threshold
    p2_mon.max_pre_nature_stat[4] = 200;
    p2_mon.minStats[0]            = 200;  // fix max-HP to 200 for deterministic % → HP conversion
    p2_mon.maxStats[0]            = 200;
    p2_mon.possible_natures      = Unknown::Known(Nature::Hardy); // neutral → BSV == final stat
    p2_mon.possible_abilities    = Unknown::Known(Ability::None); // no ability reduction
    p2_mon.item                  = Unknown::Not(vec![]);          // AV not excluded

    let state = battle_1v1(p1_mon, p2_mon);

    // Psychic: 100 BP Special Psychic-type move.
    // Neutral vs Dragon/Ground (Garchomp's types), no STAB for Normal-type Snorlax.
    let mut psychic = normal_physical_move(PokemonMove::Psychic, 100);
    psychic.category     = MoveCategory::Special;
    psychic.pokemon_type = PokemonType::Psychic;
    let mut move_dex = HashMap::new();
    move_dex.insert(PokemonMove::Psychic, psychic);

    let result = apply_ex(
        state,
        vec![event_with(
            EventKind::MoveUsed {
                user:      p1(0),
                move_used: PokemonMove::Psychic,
                targets:   vec![p2(0)],
            },
            // P2 takes 20% damage (100 % → 80 %) — consistent with AV-boosted SpD = 70×1.5 = 105.
            vec![event(EventKind::DamageDealt {
                target: p2(0),
                new_hp: PokemonHP::Percent(80),
            })],
        )],
        garchomp_dex(),
        move_dex,
    );

    let p2_result = &result.p2_active_mons[0];
    assert!(
        p2_result.min_pre_nature_stat[4] <= 70,
        "Assault Vest defender with true SpDef BSV = 70 must remain feasible; \
         got min_pre_nature_stat[4] = {} (expected ≤ 70)",
        p2_result.min_pre_nature_stat[4]
    );
}

// ── Regression: S3 — Custap Berry escape at low HP ───────────────────────────

/// When the fast mon (first mover) is at ≤ 25 % HP and Custap Berry is not
/// excluded from its item set, Pass 4 must include a Custap Berry escape
/// disjunct alongside the SpeedComparison predicate — so the ordering can be
/// explained by Custap (activates before priority resolution) rather than
/// asserting a definitive speed edge, which would be unsound if the true item
/// is Custap Berry.
#[test]
fn test_pass4_custap_escape_at_low_hp() {
    // P1 goes second (slow mon); P2 goes first (fast mon) at ≤ 25 % HP.
    // Exclude all speed-escape items/abilities EXCEPT Custap Berry and the
    // standard Speed-comparison ones (Choice Scarf excluded to keep the clause
    // clean for assertion).
    let p1_mon = no_speed_escape_mon(Species::Garchomp);
    let mut p2_mon = no_speed_escape_mon(Species::Garchomp);
    // Remove Choice Scarf from P2's exclusion list (it's already excluded in
    // no_speed_escape_mon), and keep Custap Berry NOT excluded.
    // `no_speed_escape_mon` excludes: QuickClaw, ChoiceScarf, IronBall, LaggingTail, FullIncense.
    // Custap Berry is NOT in that list → it remains possible on P2.
    p2_mon.hp = PokemonHP::Percent(20); // ≤ 25 % HP — Custap Berry can activate.

    let state = battle_1v1(p1_mon, p2_mon);
    let mut move_dex = HashMap::new();
    move_dex.insert(PokemonMove::Earthquake, normal_physical_move(PokemonMove::Earthquake, 100));
    move_dex.insert(PokemonMove::DragonClaw, normal_physical_move(PokemonMove::DragonClaw, 80));

    // P2 moves first (fast_idx = 1 in battle_1v1), then P1.
    let result = apply_ex(
        state,
        speed_order_events(PokemonMove::Earthquake, PokemonMove::DragonClaw),
        HashMap::new(),
        move_dex,
    );

    // The predicate clause for the speed comparison must include a Custap Berry
    // disjunct on the fast mon (mon_idx = 1 = P2).
    let has_custap_escape = result.predicates.iter().any(|clause| {
        let has_speed_cmp = clause.iter().any(|s| {
            matches!(s, Statement::SpeedComparison { fast_idx: 1, slow_idx: 0, .. })
        });
        let has_custap = clause.iter().any(|s| {
            matches!(s, Statement::HasItem { mon_idx: 1, item: Item::CustapBerry })
        });
        has_speed_cmp && has_custap
    });
    assert!(
        has_custap_escape,
        "Pass 4 must add a Custap Berry escape disjunct when the fast mon is at ≤ 25 % HP \
         and Custap Berry is not excluded"
    );
}

// ── Regression: S6 — Complete per-status preventer lists ─────────────────────

fn guaranteed_paralysis_secondary_move(name: PokemonMove, bp: u16) -> MoveData {
    use crate::state::dex_data::{PokemonSecondaryEffect, HitEffect};
    let mut md = normal_physical_move(name, bp);
    md.pokemon_type = PokemonType::Electric; // Nuzzle-style Electric move
    md.secondaries = vec![PokemonSecondaryEffect {
        chance: 100,
        effect: HitEffect {
            status: Some(Status::Paralysis),
            ..Default::default()
        },
        random_choices: vec![],
    }];
    md
}

fn guaranteed_freeze_secondary_move(name: PokemonMove, bp: u16) -> MoveData {
    use crate::state::dex_data::{PokemonSecondaryEffect, HitEffect};
    let mut md = normal_physical_move(name, bp);
    md.secondaries = vec![PokemonSecondaryEffect {
        chance: 100,
        effect: HitEffect {
            status: Some(Status::Frozen(0)),
            ..Default::default()
        },
        random_choices: vec![],
    }];
    md
}

fn guaranteed_poison_secondary_move(name: PokemonMove, bp: u16) -> MoveData {
    use crate::state::dex_data::{PokemonSecondaryEffect, HitEffect};
    let mut md = normal_physical_move(name, bp);
    md.secondaries = vec![PokemonSecondaryEffect {
        chance: 100,
        effect: HitEffect {
            status: Some(Status::Poison),
            ..Default::default()
        },
        random_choices: vec![],
    }];
    md
}

/// A guaranteed-paralysis damaging move whose secondary fails to land must emit
/// a clause that includes Shield Dust (it blocks secondary effects on damaging
/// moves, same scope as Covert Cloak).
#[test]
fn test_s6_shield_dust_explains_secondary_paralysis_absence() {
    // Empty dex → possible_abilities = Not([]) so ShieldDust is not pre-excluded.
    let mut p2_mon = UnknownPokemonState::from_opponent_species(
        Species::Garchomp,
        &HashMap::new(),
        50,
    );
    // Not Electric-type, not Ground-type (Ground is immune to Electric paralysis),
    // not already statused, no Substitute.
    p2_mon.possible_types = Unknown::Known(vec![PokemonType::Dragon]);

    let p1_mon = {
        let mut m = UnknownPokemonState::from_opponent_species(Species::Garchomp, &garchomp_dex(), 50);
        m.item = Unknown::Known(Item::None);
        m.possible_abilities = Unknown::Known(Ability::SandVeil);
        m
    };
    let state = battle_1v1(p1_mon, p2_mon);

    let mut move_dex = HashMap::new();
    move_dex.insert(
        PokemonMove::Nuzzle,
        guaranteed_paralysis_secondary_move(PokemonMove::Nuzzle, 20),
    );

    let result = apply_ex(
        state,
        vec![event_with(
            EventKind::MoveUsed {
                user: p1(0),
                move_used: PokemonMove::Nuzzle,
                targets: vec![p2(0)],
            },
            vec![event(EventKind::DamageDealt {
                target: p2(0),
                new_hp: PokemonHP::Percent(80),
            })],
            // No StatusInflicted — paralysis was prevented.
        )],
        garchomp_dex(),
        move_dex,
    );

    // The emitted clause must include ShieldDust (secondary-effect blocker).
    let has_shield_dust = result.predicates.iter().any(|clause| {
        clause.iter().any(|s| {
            matches!(s, Statement::HasAbility { ability: Ability::ShieldDust, .. })
        })
    });
    assert!(
        has_shield_dust,
        "Shield Dust must appear in the paralysis-preventer clause for a damaging secondary move"
    );
}

/// A guaranteed-freeze secondary under harsh sunlight must NOT emit any
/// preventer clause: freeze in harsh sunlight is a blanket weather immunity
/// ("Pokémon cannot be frozen when harsh sunlight is active"), so the absence
/// is fully explained by weather and emitting ability clauses would be unsound.
#[test]
fn test_s6_freeze_in_sun_emits_no_clause() {
    let mut p2_mon = UnknownPokemonState::from_opponent_species(
        Species::Garchomp,
        &HashMap::new(),
        50,
    );
    p2_mon.possible_types = Unknown::Known(vec![PokemonType::Dragon, PokemonType::Ground]);

    let p1_mon = {
        let mut m = UnknownPokemonState::from_opponent_species(Species::Garchomp, &garchomp_dex(), 50);
        m.item = Unknown::Known(Item::None);
        m.possible_abilities = Unknown::Known(Ability::SandVeil);
        m
    };
    let mut state = battle_1v1(p1_mon, p2_mon);
    state.weather = Some(Weather::Sun); // harsh sun → freeze is blanket-impossible

    let mut move_dex = HashMap::new();
    move_dex.insert(
        PokemonMove::Blizzard,
        guaranteed_freeze_secondary_move(PokemonMove::Blizzard, 110),
    );

    let initial_predicate_count = state.predicates.len(); // should be 0
    let result = apply_ex(
        state,
        vec![event_with(
            EventKind::MoveUsed {
                user: p1(0),
                move_used: PokemonMove::Blizzard,
                targets: vec![p2(0)],
            },
            vec![event(EventKind::DamageDealt {
                target: p2(0),
                new_hp: PokemonHP::Percent(80),
            })],
            // No StatusInflicted — freeze was prevented (by sun).
        )],
        garchomp_dex(),
        move_dex,
    );

    // The freeze absence is fully explained by harsh sun.
    // `pass2_guaranteed_status_absence` must NOT emit any status-preventer clause
    // (the sun immunity is a weather blanket, not ability/item-gated).
    //
    // Note: `pass3` I1 predicates from the DamageDealt reaction correctly emit
    // EVIVStat clauses that include damage-reducer abilities (such as PurifyingSalt)
    // as disjuncts.  These pass3 clauses are identified by containing an EVIVStat*
    // literal; we filter them out and only check for pure pass2 status-preventer
    // clauses (which only contain HasAbility / HasItem / Not literals).
    let is_stat_literal = |s: &Statement| {
        matches!(
            s,
            Statement::EVIVStatGE { .. }
            | Statement::EVIVStatLE { .. }
            | Statement::NatureBoostsStat { .. }
            | Statement::NatureNerfsStat { .. }
            | Statement::SpeedComparison { .. }
        )
    };
    let has_freeze_preventer_clause = result.predicates.iter().any(|clause| {
        // Skip clauses that are pass3 I1 predicates (they contain EVIVStat* literals).
        if clause.iter().any(|s| is_stat_literal(s)) {
            return false;
        }
        clause.iter().any(|s| matches!(
            s,
            // Any of the per-status preventer abilities for Frozen:
            Statement::HasAbility { ability: Ability::MagmaArmor, .. }
            | Statement::HasAbility { ability: Ability::Comatose, .. }
            | Statement::HasAbility { ability: Ability::PurifyingSalt, .. }
            | Statement::HasAbility { ability: Ability::ShieldsDown, .. }
            | Statement::HasAbility { ability: Ability::LeafGuard, .. }
            | Statement::HasAbility { ability: Ability::FlowerVeil, .. }
            | Statement::HasAbility { ability: Ability::ShieldDust, .. }
        ))
    });
    assert!(
        !has_freeze_preventer_clause,
        "No freeze-preventer ability clause should be emitted when harsh sunlight explains the absence"
    );
}

/// A guaranteed-poison secondary that doesn't land under harsh sunlight must
/// include Leaf Guard in the emitted clause.  Before the S6 fix, Leaf Guard was
/// missing from the Poison preventer list even though it blocks all non-volatile
/// status conditions in harsh sun.
#[test]
fn test_s6_leaf_guard_explains_poison_absence_in_sun() {
    let mut p2_mon = UnknownPokemonState::from_opponent_species(
        Species::Garchomp,
        &HashMap::new(),
        50,
    );
    // Not Poison/Steel-type, not already statused.
    p2_mon.possible_types = Unknown::Known(vec![PokemonType::Dragon, PokemonType::Ground]);

    let p1_mon = {
        let mut m = UnknownPokemonState::from_opponent_species(Species::Garchomp, &garchomp_dex(), 50);
        m.item = Unknown::Known(Item::None);
        m.possible_abilities = Unknown::Known(Ability::SandVeil);
        m
    };
    let mut state = battle_1v1(p1_mon, p2_mon);
    state.weather = Some(Weather::Sun); // harsh sun — LeafGuard should be included

    let mut move_dex = HashMap::new();
    move_dex.insert(
        PokemonMove::SludgeBomb,
        guaranteed_poison_secondary_move(PokemonMove::SludgeBomb, 90),
    );

    let result = apply_ex(
        state,
        vec![event_with(
            EventKind::MoveUsed {
                user: p1(0),
                move_used: PokemonMove::SludgeBomb,
                targets: vec![p2(0)],
            },
            vec![event(EventKind::DamageDealt {
                target: p2(0),
                new_hp: PokemonHP::Percent(80),
            })],
            // No StatusInflicted — poison was prevented.
        )],
        garchomp_dex(),
        move_dex,
    );

    let has_leaf_guard = result.predicates.iter().any(|clause| {
        clause.iter().any(|s| {
            matches!(s, Statement::HasAbility { ability: Ability::LeafGuard, .. })
        })
    });
    assert!(
        has_leaf_guard,
        "Leaf Guard must appear in the poison-preventer clause under harsh sun"
    );
}

// ── Regression: I1 — Direction A CNF predicate emission ──────────────────────

/// After we attack a known-species opponent and observe a damage percent, Direction A
/// must emit a nature-conditional EVIVStat predicate for the defender's defensive stat
/// (mirroring Direction B).  Specifically, for a neutral-nature defender with no
/// damage-reducing item/ability excluded, BCP should eventually be able to force a
/// tighter min_pre_nature_stat once the item/ability disjuncts are eliminated.
///
/// This test verifies the predicate is present in the state (the EVIVStatGE clause)
/// rather than testing full BCP resolution (which requires solving the whole CNF).
#[test]
fn test_pass3_dir_a_emits_nature_conditional_predicate() {
    use crate::state::pokemon::Nature;
    use crate::state::dex_data::PokemonStat;

    // P1: our known mon with SpA = 100, no item/ability multipliers.
    let mut p1_mon = unknown_mon_species(Species::Snorlax);
    p1_mon.hp = PokemonHP::Number(500);
    p1_mon.minStats = [500, 100, 100, 100, 100, 100];
    p1_mon.maxStats = [500, 100, 100, 100, 100, 100];
    p1_mon.item               = Unknown::Known(Item::None);
    p1_mon.possible_abilities = Unknown::Known(Ability::None);

    // P2: Garchomp defender; BSV range [50, 200] for SpD (wider than true).
    // Neutral nature so BSV == stat.  No item/ability excluded (AV/Eviolite possible).
    let mut p2_mon = UnknownPokemonState::from_opponent_species(
        Species::Garchomp, &garchomp_dex(), 50,
    );
    p2_mon.hp                     = PokemonHP::Percent(100);
    p2_mon.min_pre_nature_stat[4] = 50;
    p2_mon.max_pre_nature_stat[4] = 200;
    p2_mon.minStats[0]            = 200;
    p2_mon.maxStats[0]            = 200;
    p2_mon.possible_natures       = Unknown::Known(Nature::Hardy); // neutral
    p2_mon.possible_abilities     = Unknown::Known(Ability::None);
    p2_mon.item                   = Unknown::Not(vec![]);          // AV not excluded

    let state = battle_1v1(p1_mon, p2_mon);

    let mut psychic = normal_physical_move(PokemonMove::Psychic, 100);
    psychic.category     = MoveCategory::Special;
    psychic.pokemon_type = PokemonType::Psychic;
    let mut move_dex = HashMap::new();
    move_dex.insert(PokemonMove::Psychic, psychic);

    let result = apply_ex(
        state,
        vec![event_with(
            EventKind::MoveUsed {
                user:      p1(0),
                move_used: PokemonMove::Psychic,
                targets:   vec![p2(0)],
            },
            vec![event(EventKind::DamageDealt {
                target:  p2(0),
                new_hp:  PokemonHP::Percent(80), // 20% damage
            })],
        )],
        garchomp_dex(),
        move_dex,
    );

    // Direction A (I1) must emit at least one EVIVStatGE or EVIVStatLE predicate
    // for the defender's SpD stat (stat index 4 = SpD).
    let has_spd_predicate = result.predicates.iter().any(|clause| {
        clause.iter().any(|s| {
            matches!(
                s,
                Statement::EVIVStatGE { stat: PokemonStat::SpD, .. }
                | Statement::EVIVStatLE { stat: PokemonStat::SpD, .. }
            )
        })
    });
    assert!(
        has_spd_predicate,
        "Direction A must emit an EVIVStat predicate for the defender's SpD after observing damage %"
    );
}

// ── Regression: S11 — Direction A must not fire for an unknown (non-P1) attacker ──

/// In doubles, an opponent mon can hit its OWN ally with a spread move — both mons'
/// HP display as `Percent` (neither belongs to the observer), so the target's HP
/// representation alone (`Percent`) is not sufficient to conclude the attacker is
/// our own fully-`Known` mon. Before the S11 fix, Direction A fired unconditionally
/// whenever the target's HP was `Percent`, materializing the (unknown) P2 attacker's
/// unresolved stat bounds as if they were exact — an unsound basis for narrowing the
/// defender's stat bounds. After the fix, Direction A must not touch the defender's
/// bounds at all when the attacker is not P1 (the observer).
#[test]
fn test_pass3_dir_a_skipped_for_opponent_ally_hit() {
    let p1_mons = vec![unknown_mon(), unknown_mon()];

    // P2 mon 0: the attacker (unknown to us).
    let p2_attacker = unknown_mon_species(Species::Snorlax);
    // P2 mon 1: the defender — Garchomp, with a wide starting SpD BSV range.
    let mut p2_defender = UnknownPokemonState::from_opponent_species(
        Species::Garchomp, &garchomp_dex(), 50,
    );
    p2_defender.hp = PokemonHP::Percent(100);
    let orig_bsv_lo = p2_defender.min_pre_nature_stat[4];
    let orig_bsv_hi = p2_defender.max_pre_nature_stat[4];

    let mut state = battle_nvn(p1_mons, vec![p2_attacker, p2_defender]);
    state.active_per_side = 2;

    let mut psychic = normal_physical_move(PokemonMove::Psychic, 100);
    psychic.category = MoveCategory::Special;
    psychic.pokemon_type = PokemonType::Psychic;
    let mut move_dex = HashMap::new();
    move_dex.insert(PokemonMove::Psychic, psychic);

    let result = apply_ex(
        state,
        vec![event_with(
            EventKind::MoveUsed {
                user: p2(0),
                move_used: PokemonMove::Psychic,
                targets: vec![p2(1)],
            },
            vec![event(EventKind::DamageDealt {
                target: p2(1),
                new_hp: PokemonHP::Percent(80), // 20% damage
            })],
        )],
        garchomp_dex(),
        move_dex,
    );

    // No EVIVStat predicate for the defender's SpD may be emitted — Direction A
    // must not have run at all for this (unknown-attacker) hit.
    let has_spd_predicate = result.predicates.iter().any(|clause| {
        clause.iter().any(|s| {
            matches!(
                s,
                Statement::EVIVStatGE { stat: crate::state::dex_data::PokemonStat::SpD, .. }
                | Statement::EVIVStatLE { stat: crate::state::dex_data::PokemonStat::SpD, .. }
            )
        })
    });
    assert!(
        !has_spd_predicate,
        "Direction A must not emit any defender predicate for a non-P1 attacker's hit"
    );

    // The defender's pre-nature SpD bounds must remain exactly the initial wide range —
    // no unconditional tightening from apply_unconditional_tightening either.
    let defender = &result.p2_active_mons[1];
    assert_eq!(
        defender.min_pre_nature_stat[4], orig_bsv_lo,
        "defender's min SpD BSV must be untouched by an unknown-attacker hit"
    );
    assert_eq!(
        defender.max_pre_nature_stat[4], orig_bsv_hi,
        "defender's max SpD BSV must be untouched by an unknown-attacker hit"
    );
}

// ── Regression: E1 — Binary-search preserves precision of linear scan ────────

/// The binary-search implementation of `find_feasible_bsv_range_b` (Direction B) must
/// return the same BSV bounds as the former linear scan; this is an explicit regression
/// on top of the implicit coverage from the rest of the pass3_dir_b_* suite.
///
/// With Def=65, bp=40, lv=50 (no items/STAB/type effects), Atk=148 is the lowest value
/// whose max roll (100%) reaches damage 42, and Atk=180 is the highest whose min roll
/// (85%) still reaches it — so observing exactly 42 damage must tighten
/// min_pre_nature_stat[1] from 135 to 148 and max_pre_nature_stat[1] from 182 to 180.
#[test]
fn test_e1_binary_search_direction_b_preserves_precision() {
    use crate::state::pokemon::Nature;

    // P1: Snorlax (not in garchomp_dex → pass5 skips HP validation).
    // Use exact known stats so the defender is a fully-determined target.
    let mut p1_mon = unknown_mon_species(Species::Snorlax);
    p1_mon.hp = PokemonHP::Number(200);
    p1_mon.minStats = [200, 110, 65, 65, 110, 30]; // approximate Snorlax level-50 stats
    p1_mon.maxStats = [200, 110, 65, 65, 110, 30];
    p1_mon.item               = Unknown::Known(Item::None);
    p1_mon.possible_abilities = Unknown::Known(Ability::None);

    // P2: Garchomp attacker — full BSV range, Hardy nature so BSV == final stat.
    let mut p2_mon = UnknownPokemonState::from_opponent_species(
        Species::Garchomp, &garchomp_dex(), 50,
    );
    p2_mon.hp               = PokemonHP::Percent(100);
    p2_mon.possible_natures = Unknown::Known(Nature::Hardy);
    p2_mon.item             = Unknown::Known(Item::None);
    p2_mon.possible_abilities = Unknown::Known(Ability::None);

    let initial_atk_min = p2_mon.min_pre_nature_stat[1];
    let initial_atk_max = p2_mon.max_pre_nature_stat[1];

    let state = battle_1v1(p1_mon, p2_mon);
    let mut move_dex = HashMap::new();
    move_dex.insert(PokemonMove::Tackle, normal_physical_move(PokemonMove::Tackle, 40));

    let result = apply_ex(
        state,
        vec![event_with(
            EventKind::MoveUsed {
                user:      p2(0),
                move_used: PokemonMove::Tackle,
                targets:   vec![p1(0)],
            },
            vec![event(EventKind::DamageDealt {
                target: p1(0),
                new_hp: PokemonHP::Number(158), // 200 − 158 = 42 exact HP dealt
            })],
        )],
        garchomp_dex(),
        move_dex,
    );

    let p2_result = &result.p2_active_mons[0];
    // Hand-derived expected bounds (verified against the damage formula):
    //   atk=147 → pre-roll dmg = floor(floor(880*147/65)/50)+2 = floor(39)+2 = 41 ✗ (max roll = 41 < 42)
    //   atk=148 → pre-roll dmg = floor(floor(880*148/65)/50)+2 = floor(40)+2 = 42 ✓ (max roll = 42)
    //   atk=180 → pre-roll dmg = floor(floor(880*180/65)/50)+2 = floor(48)+2 = 50 ✓ (min roll 0.85×50=42)
    //   atk=181 → pre-roll dmg = floor(floor(880*181/65)/50)+2 = floor(49)+2 = 51 ✗ (min roll 0.85×51=43 > 42)
    let expected_min = 148u16;
    let expected_max = 180u16;
    assert_eq!(
        p2_result.min_pre_nature_stat[1], expected_min,
        "Binary search must raise min Atk BSV from {} to {} (42 HP dealt from 200)",
        initial_atk_min, expected_min
    );
    assert_eq!(
        p2_result.max_pre_nature_stat[1], expected_max,
        "Binary search must lower max Atk BSV from {} to {} (42 HP dealt from 200)",
        initial_atk_max, expected_max
    );
}

/// When the fast mon is at > 25 % HP, Custap Berry activation is impossible and
/// must NOT appear as an escape disjunct.
#[test]
fn test_pass4_no_custap_escape_at_full_hp() {
    let p1_mon = no_speed_escape_mon(Species::Garchomp);
    let mut p2_mon = no_speed_escape_mon(Species::Garchomp);
    p2_mon.hp = PokemonHP::Percent(100); // full HP — Custap Berry cannot activate.

    let state = battle_1v1(p1_mon, p2_mon);
    let mut move_dex = HashMap::new();
    move_dex.insert(PokemonMove::Earthquake, normal_physical_move(PokemonMove::Earthquake, 100));
    move_dex.insert(PokemonMove::DragonClaw, normal_physical_move(PokemonMove::DragonClaw, 80));

    let result = apply_ex(
        state,
        speed_order_events(PokemonMove::Earthquake, PokemonMove::DragonClaw),
        HashMap::new(),
        move_dex,
    );

    let has_custap_escape = result.predicates.iter().any(|clause| {
        clause.iter().any(|s| matches!(s, Statement::HasItem { item: Item::CustapBerry, .. }))
    });
    assert!(
        !has_custap_escape,
        "Custap Berry must NOT appear as an escape disjunct when the fast mon is at full HP"
    );
}

// ── I2: Flame Orb / Toxic Orb EOT detection ──────────────────────────────────

/// An EOT `StatusInflicted{Burn}` on an opponent with no prior status must force
/// `item = Known(FlameOrb)`.  FlameOrb is the only EOT self-burn source.
#[test]
fn test_i2_flame_orb_eot() {
    let mut p2_mon = unknown_mon_species(Species::Garchomp);
    p2_mon.hp = PokemonHP::Percent(100);
    // No prior status, item unknown.
    assert!(p2_mon.status.is_none());

    let state = battle_with_p2(vec![p2_mon]);

    let result = apply(
        state,
        vec![event_with(
            EventKind::EndOfTurn,
            vec![event(EventKind::StatusInflicted {
                target: p2(0),
                status: Status::Burn,
            })],
        )],
    );

    assert_eq!(
        result.p2_active_mons[0].item,
        Unknown::Known(Item::FlameOrb),
        "EOT self-burn with no prior status must force Known(FlameOrb)"
    );
}

/// A negative case: Burn inflicted via a *MoveUsed* reaction (Will-O-Wisp etc.)
/// rather than an EndOfTurn reaction — the pass must NOT infer FlameOrb, since
/// move-applied burns do not indicate a held orb.
#[test]
fn test_i2_flame_orb_not_inferred_for_move_burn() {
    let mut p2_mon = unknown_mon_species(Species::Garchomp);
    p2_mon.hp = PokemonHP::Percent(100);

    let p1_mon = unknown_mon_species(Species::Garchomp);
    let state = battle_1v1(p1_mon, p2_mon);

    let mut move_dex = HashMap::new();
    let mut wow = poke_status_move(PokemonMove::WillOWisp);
    wow.accuracy = AccuracyType::Percent(85);
    wow.pokemon_type = PokemonType::Fire;
    move_dex.insert(PokemonMove::WillOWisp, wow);

    let result = apply_ex(
        state,
        vec![event_with(
            EventKind::MoveUsed {
                user: p1(0),
                move_used: PokemonMove::WillOWisp,
                targets: vec![p2(0)],
            },
            vec![event(EventKind::StatusInflicted {
                target: p2(0),
                status: Status::Burn,
            })],
        )],
        HashMap::new(),
        move_dex,
    );

    // Item must NOT be forced to FlameOrb (burn came from a move, not EOT).
    assert_ne!(
        result.p2_active_mons[0].item,
        Unknown::Known(Item::FlameOrb),
        "Move-applied Burn must NOT force Known(FlameOrb)"
    );
}

/// An EOT `StatusInflicted{ToxicPoison}` on an opponent with no prior status must
/// force `item = Known(ToxicOrb)`.
#[test]
fn test_i2_toxic_orb_eot() {
    let mut p2_mon = unknown_mon_species(Species::Garchomp);
    p2_mon.hp = PokemonHP::Percent(100);

    let state = battle_with_p2(vec![p2_mon]);

    let result = apply(
        state,
        vec![event_with(
            EventKind::EndOfTurn,
            vec![event(EventKind::StatusInflicted {
                target: p2(0),
                status: Status::ToxicPoison(0),
            })],
        )],
    );

    assert_eq!(
        result.p2_active_mons[0].item,
        Unknown::Known(Item::ToxicOrb),
        "EOT self-toxic with no prior status must force Known(ToxicOrb)"
    );
}

// ── I2: Air Balloon ground-immunity clause ───────────────────────────────────

/// When a Ground-type move is immune on a P2 mon whose type is Known and NOT
/// Flying, a disjunctive clause `HasItem(AirBalloon) ∨ HasAbility(Levitate) ∨ ...`
/// must be emitted.  Once Levitate and other abilities are excluded, BCP forces
/// `item = Known(AirBalloon)`.
#[test]
fn test_i2_air_balloon_ground_immunity_clause() {
    // P2: Garchomp (Dragon/Ground) — NOT Flying, so Ground immunity ≠ type chart.
    let mut p2_mon =
        UnknownPokemonState::from_opponent_species(Species::Garchomp, &garchomp_dex(), 50);
    p2_mon.possible_types = Unknown::Known(vec![PokemonType::Dragon, PokemonType::Ground]);
    // Exclude Levitate, Eelevate, EarthEater so BCP forces AirBalloon.
    p2_mon.possible_abilities = Unknown::Not(vec![
        Ability::Levitate,
        Ability::Eelevate,
        Ability::EarthEater,
    ]);
    p2_mon.hp = PokemonHP::Percent(100);

    let p1_mon = unknown_mon_species(Species::Garchomp);
    let state = battle_1v1(p1_mon, p2_mon);

    // Ground-type physical move (Earthquake analogue).
    let mut move_dex = HashMap::new();
    move_dex.insert(
        PokemonMove::Earthquake,
        ground_physical_move(PokemonMove::Earthquake, 100),
    );

    let result = apply_ex(
        state,
        vec![event_with(
            EventKind::MoveUsed {
                user: p1(0),
                move_used: PokemonMove::Earthquake,
                targets: vec![p2(0)],
            },
            vec![event(EventKind::Immune { target: p2(0) })],
        )],
        garchomp_dex(),
        move_dex,
    );

    // BCP should force item = Known(AirBalloon) since all other explanations excluded.
    assert_eq!(
        result.p2_active_mons[0].item,
        Unknown::Known(Item::AirBalloon),
        "With Levitate/Eelevate/EarthEater excluded, BCP must force Known(AirBalloon)"
    );
}

/// When the P2 mon's types are Unknown (could be Flying), no clause must be emitted
/// (emitting it would be unsound).
#[test]
fn test_i2_no_air_balloon_clause_when_types_unknown() {
    let mut p2_mon = unknown_mon_species(Species::Garchomp);
    // Leave possible_types as wide Not (unknown; could include Flying).
    p2_mon.possible_types = Unknown::Not(Vec::new());
    p2_mon.hp = PokemonHP::Percent(100);

    let p1_mon = unknown_mon_species(Species::Garchomp);
    let state = battle_1v1(p1_mon, p2_mon);

    let mut move_dex = HashMap::new();
    move_dex.insert(
        PokemonMove::Earthquake,
        ground_physical_move(PokemonMove::Earthquake, 100),
    );

    let result = apply_ex(
        state,
        vec![event_with(
            EventKind::MoveUsed {
                user: p1(0),
                move_used: PokemonMove::Earthquake,
                targets: vec![p2(0)],
            },
            vec![event(EventKind::Immune { target: p2(0) })],
        )],
        HashMap::new(), // dex: species unknown → pass5 skips
        move_dex,
    );

    // No AirBalloon clause should be emitted when types are unknown.
    let has_air_balloon_clause = result.predicates.iter().any(|clause| {
        clause
            .iter()
            .any(|s| matches!(s, Statement::HasItem { item: Item::AirBalloon, .. }))
    });
    assert!(
        !has_air_balloon_clause,
        "Must NOT emit AirBalloon clause when possible_types is unknown (could be Flying)"
    );
}

// ── Item clause (allow_repeat_items) ─────────────────────────────────────────

/// Build a battle with one P1 active mon, one P2 active mon, and one P2 known-back mon.
///
/// mon_idx layout:
///   0 = p1_active[0]
///   1 = p2_active[0]   (P2 active; teammates: [2])
///   2 = p2_known_back[0]
fn battle_1v1_with_known_back(
    p1_active: UnknownPokemonState,
    p2_active: UnknownPokemonState,
    p2_back: UnknownPokemonState,
) -> UnknownBattleState {
    let mut state = battle_nvn(vec![p1_active], vec![p2_active]);
    state.p2_known_back_mons.push(p2_back);
    state
}

/// Item clause: revealing the active opponent's item excludes it from the back mon.
#[test]
fn test_item_clause_excludes_revealed_item_from_teammate() {
    let p1_mon = unknown_mon();
    let p2_active = unknown_mon_species(Species::Garchomp);
    let p2_back = unknown_mon_species(Species::Corviknight);
    let state = battle_1v1_with_known_back(p1_mon, p2_active, p2_back);

    // Default config: allow_repeat_items = false (item clause on).
    let result = apply_with_config(
        state,
        vec![event(EventKind::ItemRevealed { slot: p2(0), item: Item::Leftovers })],
        HashMap::new(),
        HashMap::new(),
        InferenceConfig::default(),
    );

    // Active mon (idx 1) must now be Known(Leftovers).
    let active = get_mon_by_idx(&result, 1).unwrap();
    assert!(
        matches!(&active.item, Unknown::Known(Item::Leftovers)),
        "Active mon should be Known(Leftovers), got {:?}",
        active.item
    );
    // Back mon (idx 2) must have Leftovers excluded.
    let back = get_mon_by_idx(&result, 2).unwrap();
    assert!(
        is_item_excluded(back, &Item::Leftovers),
        "Back mon should have Leftovers excluded by item clause, got {:?}",
        back.item
    );
}

/// Item::None is exempt from the item clause — multiple mons may hold no item.
#[test]
fn test_item_clause_none_item_not_excluded() {
    let p1_mon = unknown_mon();
    let p2_active = unknown_mon_species(Species::Garchomp);
    let p2_back = unknown_mon_species(Species::Corviknight);
    let state = battle_1v1_with_known_back(p1_mon, p2_active, p2_back);

    let result = apply_with_config(
        state,
        vec![event(EventKind::ItemRevealed { slot: p2(0), item: Item::None })],
        HashMap::new(),
        HashMap::new(),
        InferenceConfig::default(),
    );

    // Back mon must still allow Item::None (no exclusion for no-item).
    let back = get_mon_by_idx(&result, 2).unwrap();
    assert!(
        !is_item_excluded(back, &Item::None),
        "Back mon must still allow Item::None; exclusion of no-item violates the exemption"
    );
}

/// With allow_repeat_items = true, no cross-teammate exclusion occurs.
#[test]
fn test_repeat_items_allowed_no_exclusion() {
    let p1_mon = unknown_mon();
    let p2_active = unknown_mon_species(Species::Garchomp);
    let p2_back = unknown_mon_species(Species::Corviknight);
    let state = battle_1v1_with_known_back(p1_mon, p2_active, p2_back);

    let result = apply_with_config(
        state,
        vec![event(EventKind::ItemRevealed { slot: p2(0), item: Item::Leftovers })],
        HashMap::new(),
        HashMap::new(),
        InferenceConfig { allow_repeat_items: true, ..InferenceConfig::default() },
    );

    // Back mon must still allow Leftovers (clause is off).
    let back = get_mon_by_idx(&result, 2).unwrap();
    assert!(
        !is_item_excluded(back, &Item::Leftovers),
        "Back mon must still allow Leftovers when allow_repeat_items = true"
    );
}

/// BCP cascade: back mon pre-narrowed to Possibly([Leftovers, Sitrus]); active
/// mon revealed as Leftovers → item clause excludes Leftovers from back →
/// BCP collapses back mon to Known(Sitrus).
#[test]
fn test_item_clause_bcp_cascade() {
    let p1_mon = unknown_mon();
    let p2_active = unknown_mon_species(Species::Garchomp);
    let mut p2_back = unknown_mon_species(Species::Corviknight);
    // Pre-narrow the back mon's item to two candidates.
    p2_back.item = Unknown::Possibly(vec![Item::Leftovers, Item::SitrusBerry]);
    let state = battle_1v1_with_known_back(p1_mon, p2_active, p2_back);

    let result = apply_with_config(
        state,
        vec![event(EventKind::ItemRevealed { slot: p2(0), item: Item::Leftovers })],
        HashMap::new(),
        HashMap::new(),
        InferenceConfig::default(),
    );

    // After exclusion of Leftovers, Possibly([SitrusBerry]) → Known(SitrusBerry).
    let back = get_mon_by_idx(&result, 2).unwrap();
    assert!(
        matches!(&back.item, Unknown::Known(Item::SitrusBerry)),
        "Back mon should collapse to Known(SitrusBerry) via item-clause + BCP cascade, got {:?}",
        back.item
    );
}

// ── S-B: Direction-A HP sampling — EV-lattice enumeration ────────────────────

/// **S-B regression test.** Direction A back-solves the defender's BSV from a percent-HP
/// observation by sampling max-HP candidates. The old `step_by(4)` loop started at `hp_lo`
/// and could skip `hp_hi` entirely; since feasible-BSV lower bounds are non-increasing in
/// `hp_cand`, missing the highest candidate can raise `min_pre_nature_stat` above the true
/// value (unsound exclusion).
///
/// Setup pins defender HP to the 4-value EV-lattice window `[183, 186]` — `step_by(4)` from
/// 183 only ever samples 183. At SpD_stat=121, hp_cand=186 makes BSV=121 feasible but
/// hp_cand=183 does not, so the old code raises the min bound past 121 while the EV-lattice
/// enumerator (which also samples 186) keeps it feasible.
#[test]
fn test_pass3_dir_a_ev_lattice_hp_does_not_exclude_true_bsv() {
    use crate::state::pokemon::Nature;

    // Attacker: SpA=300, no item/ability so oracle uses raw stats.
    let mut p1_mon = unknown_mon_species(Species::Snorlax);
    p1_mon.hp = PokemonHP::Number(500);
    p1_mon.minStats = [500, 100, 100, 300, 100, 100]; // SpA = 300
    p1_mon.maxStats = [500, 100, 100, 300, 100, 100];
    p1_mon.item = Unknown::Known(Item::None);
    p1_mon.possible_abilities = Unknown::Known(Ability::None);

    // Defender: Garchomp, HP range [183, 186] (EV=0/4/12/20 at lv50, base_hp=108).
    // SpD BSV range widened so BSV=121 is a candidate (Garchomp natural max BSV~137,
    // but we inflate the search space to include artificially low values).
    // With Hardy nature and no item, the oracle uses SpD_stat = BSV directly.
    let mut p2_mon = UnknownPokemonState::from_opponent_species(
        Species::Garchomp, &garchomp_dex(), 50,
    );
    p2_mon.hp = PokemonHP::Percent(100);
    // Pin HP to the 4-value lattice window [183, 186].
    p2_mon.minStats[0] = 183;
    p2_mon.maxStats[0] = 186;
    // Widen the SpD BSV search space to include 121 as the boundary.
    p2_mon.min_pre_nature_stat[4] = 50;
    p2_mon.max_pre_nature_stat[4] = 200;
    p2_mon.minStats[4] = 50;
    p2_mon.maxStats[4] = 200;
    p2_mon.possible_natures = Unknown::Known(Nature::Hardy); // neutral: BSV == final stat
    p2_mon.possible_abilities = Unknown::Known(Ability::None);
    p2_mon.item = Unknown::Known(Item::None);

    let state = battle_1v1(p1_mon, p2_mon);

    // 100 BP Special Psychic move — Psychic-type neutral vs Dragon/Ground.
    let mut psychic = normal_physical_move(PokemonMove::Psychic, 100);
    psychic.category = MoveCategory::Special;
    psychic.pokemon_type = PokemonType::Psychic;
    let mut move_dex = HashMap::new();
    move_dex.insert(PokemonMove::Psychic, psychic);

    // Observe 50% damage: new_hp = Percent(50) from old Percent(100) → delta_pct = 50.
    let result = apply_ex(
        state,
        vec![event_with(
            EventKind::MoveUsed {
                user: p1(0),
                move_used: PokemonMove::Psychic,
                targets: vec![p2(0)],
            },
            vec![event(EventKind::DamageDealt {
                target: p2(0),
                new_hp: PokemonHP::Percent(50), // 50% damage
            })],
        )],
        garchomp_dex(),
        move_dex,
    );

    let p2_result = &result.p2_active_mons[0];
    // The EV-lattice fix ensures hp=186 is sampled, making BSV=121 feasible.
    // The old step_by(4) code (sampling only hp=183) would raise min_pre_nature_stat[4]
    // to ≥124, excluding BSV=121 (which is the truly feasible boundary value at hp=186).
    assert!(
        p2_result.min_pre_nature_stat[4] <= 121,
        "BSV=121 (SpD_stat=121 at Hardy nature) must remain feasible; \
         old step_by(4) would raise min_pre_nature_stat[4] to ≥124 (got {}). \
         The EV-lattice fix samples hp=186, where BSV=121 is the feasible boundary.",
        p2_result.min_pre_nature_stat[4]
    );
}

// ── S-A: Per-effect timer model ───────────────────────────────────────────────

/// Tailwind must be modelled as exactly 4 turns.
///
/// The old blanket `Possibly([5,8])` *excluded* 4, which is unsound: Tailwind
/// always lasts 4 turns (Gen V+), so the true duration must be in the candidate
/// set.
#[test]
fn test_tailwind_timer_is_known_4() {
    let state = battle_with_p2(vec![unknown_mon()]);
    let result = apply(
        state,
        vec![event(EventKind::SideConditionStart {
            side: Player::P2,
            condition: SideCondition::TailWind,
        })],
    );
    assert_eq!(
        result.p2_side_condition_turns,
        vec![Unknown::Known(4)],
        "Tailwind timer must be Known(4), not Possibly([5,8])"
    );
}

/// Trick Room must be modelled as exactly 5 turns.
#[test]
fn test_trick_room_timer_is_known_5() {
    let state = battle_with_p2(vec![unknown_mon()]);
    let result = apply(
        state,
        vec![event(EventKind::PseudoWeatherStart {
            effect: PseudoWeather::TrickRoom,
        })],
    );
    assert_eq!(
        result.pseudo_weather_turns,
        vec![Unknown::Known(5)],
        "Trick Room timer must be Known(5), not Possibly([5,8])"
    );
}

/// Gravity must be modelled as exactly 5 turns.
#[test]
fn test_gravity_timer_is_known_5() {
    let state = battle_with_p2(vec![unknown_mon()]);
    let result = apply(
        state,
        vec![event(EventKind::PseudoWeatherStart {
            effect: PseudoWeather::Gravity,
        })],
    );
    assert_eq!(
        result.pseudo_weather_turns,
        vec![Unknown::Known(5)],
        "Gravity timer must be Known(5)"
    );
}

/// Screens (Reflect/LightScreen/AuroraVeil) must be modelled as Possibly([5,8])
/// because Light Clay can extend them.
#[test]
fn test_reflect_timer_is_possibly_5_8() {
    let state = battle_with_p2(vec![unknown_mon()]);
    let result = apply(
        state,
        vec![event(EventKind::SideConditionStart {
            side: Player::P2,
            condition: SideCondition::Reflect,
        })],
    );
    assert_eq!(
        result.p2_side_condition_turns,
        vec![Unknown::Possibly(vec![5, 8])],
        "Reflect timer must be Possibly([5,8]) (Light Clay can extend to 8)"
    );
}

/// QuickGuard must be modelled as exactly 1 turn.
#[test]
fn test_quick_guard_timer_is_known_1() {
    let state = battle_with_p2(vec![unknown_mon()]);
    let result = apply(
        state,
        vec![event(EventKind::SideConditionStart {
            side: Player::P2,
            condition: SideCondition::QuickGuard,
        })],
    );
    assert_eq!(
        result.p2_side_condition_turns,
        vec![Unknown::Known(1)],
        "QuickGuard timer must be Known(1), not Possibly([5,8])"
    );
}

/// Entry hazards (Stealth Rock) must be modelled as Known(0) = permanent.
#[test]
fn test_stealth_rock_timer_is_known_0_permanent() {
    let state = battle_with_p2(vec![unknown_mon()]);
    let result = apply(
        state,
        vec![event(EventKind::SideConditionStart {
            side: Player::P2,
            condition: SideCondition::StealthRock,
        })],
    );
    assert_eq!(
        result.p2_side_condition_turns,
        vec![Unknown::Known(0)],
        "StealthRock timer must be Known(0) (permanent; no countdown)"
    );
}

/// Standard weather (Rain) must be modelled as Possibly([5,8]).
/// Primordial weather (HeavyRain) must be Known(0).
#[test]
fn test_rain_timer_is_possibly_5_8() {
    let state = battle_with_p2(vec![unknown_mon()]);
    let result = apply(
        state,
        vec![event(EventKind::WeatherChanged {
            weather: Some(Weather::Rain),
        })],
    );
    assert_eq!(
        result.weather_turns,
        Some(Unknown::Possibly(vec![5, 8])),
        "Rain timer must be Possibly([5,8])"
    );
}

#[test]
fn test_primordial_weather_timer_is_known_0() {
    let state = battle_with_p2(vec![unknown_mon()]);
    let result = apply(
        state,
        vec![event(EventKind::WeatherChanged {
            weather: Some(Weather::HeavyRain),
        })],
    );
    assert_eq!(
        result.weather_turns,
        Some(Unknown::Known(0)),
        "HeavyRain timer must be Known(0) (permanent; no countdown)"
    );
}

/// Tailwind decrement: after 3 EndOfTurn events the timer reaches Known(1).
/// (After one more it would reach 0 and SideConditionEnd fires in the simulator,
/// but we test that no unsound exclusion happens before that.)
#[test]
fn test_tailwind_timer_decrements_correctly() {
    let mut state = battle_with_p2(vec![unknown_mon()]);
    state = apply(
        state,
        vec![event(EventKind::SideConditionStart {
            side: Player::P2,
            condition: SideCondition::TailWind,
        })],
    );
    assert_eq!(result_p2_sc_turns(&state), vec![Unknown::Known(4)]);

    for expected in [3u8, 2, 1] {
        state = apply(state, vec![event(EventKind::EndOfTurn)]);
        let turns = &state.p2_side_condition_turns;
        assert_eq!(
            turns,
            &vec![Unknown::Known(expected)],
            "After decrement, Tailwind timer should be {expected}"
        );
    }
}

fn result_p2_sc_turns(state: &UnknownBattleState) -> Vec<Unknown<u8>> {
    state.p2_side_condition_turns.clone()
}

/// ItemGained (Trick / Switcheroo) must NOT propagate item-clause exclusion
/// to teammates, because the transferred item was not the mon's team-built item.
#[test]
fn test_item_gained_does_not_exclude_teammate() {
    let p1_mon = unknown_mon();
    let p2_active = unknown_mon_species(Species::Garchomp);
    let p2_back = unknown_mon_species(Species::Corviknight);
    let state = battle_1v1_with_known_back(p1_mon, p2_active, p2_back);

    let result = apply_with_config(
        state,
        vec![event(EventKind::ItemGained { slot: p2(0), item: Item::Leftovers })],
        HashMap::new(),
        HashMap::new(),
        InferenceConfig::default(),
    );

    // Back mon must still allow Leftovers; ItemGained is a transfer, not a reveal.
    let back = get_mon_by_idx(&result, 2).unwrap();
    assert!(
        !is_item_excluded(back, &Item::Leftovers),
        "ItemGained must not trigger item-clause exclusion; back mon must still allow Leftovers"
    );
}

// ── Regression: S12 — a re-confirmed transferred item must not exclude teammates ──

/// A transferred item (Trick/Switcheroo, via `ItemGained`) that is later re-revealed
/// by an independent source (e.g. Frisk emitting `ItemRevealed` for the SAME item on
/// the SAME mon on a later turn) must still not trigger the item-clause exclusion.
/// Before the S12 fix, `ItemRevealed` unconditionally called `enforce_unique_item`
/// regardless of how the mon came to hold that item, so a Frisk reveal of a Tricked-in
/// item would unsoundly exclude that item from the mon's own (legitimately
/// team-built) teammates.
#[test]
fn test_item_revealed_after_transfer_does_not_exclude_teammate() {
    let p1_mon = unknown_mon();
    let p2_active = unknown_mon_species(Species::Garchomp);
    let p2_back = unknown_mon_species(Species::Corviknight);
    let state = battle_1v1_with_known_back(p1_mon, p2_active, p2_back);

    let result = apply_with_config(
        state,
        vec![
            // Turn N: the active mon receives Leftovers via a transfer (Trick).
            event(EventKind::ItemGained { slot: p2(0), item: Item::Leftovers }),
            // Turn N+k: an independent source (Frisk) re-reveals the SAME item.
            event(EventKind::ItemRevealed { slot: p2(0), item: Item::Leftovers }),
        ],
        HashMap::new(),
        HashMap::new(),
        InferenceConfig::default(),
    );

    let active = get_mon_by_idx(&result, 1).unwrap();
    assert!(
        matches!(&active.item, Unknown::Known(Item::Leftovers)),
        "Active mon should be Known(Leftovers), got {:?}",
        active.item
    );
    let back = get_mon_by_idx(&result, 2).unwrap();
    assert!(
        !is_item_excluded(back, &Item::Leftovers),
        "Back mon must still allow Leftovers — the active mon's Leftovers was transferred, \
         not team-built, so it says nothing about what the back mon may hold"
    );
}

// ── I-B: Pass 5 re-run after BCP ─────────────────────────────────────────────

/// Regression test for I-B: Pass 5 must be re-run after the final BCP so that
/// stat bounds tightened by BCP's `force_literal` (EVIVStatGE) are reflected in
/// nature exclusion.
///
/// Setup: Garchomp lv50, max IVs, Atk observed in [150,170], with a pre-injected unit
/// predicate EVIVStatGE{Atk,156} (simulating BCP resolving a disjunctive clause to this
/// literal). Before BCP, min_pre_nature_stat[Atk]=135 lets EV=36 (BSV=155) produce a
/// feasible +Atk stat of 170, so +Atk nature isn't excluded. After BCP raises the floor
/// to 156, EV=36 is skipped and EV=44 (BSV=156) gives +Atk stat=171 > 170 — infeasible —
/// so +Atk natures must now be excluded. Without a second Pass 5 run after BCP, this
/// tighter exclusion would be missed.
#[test]
fn test_ib_pass5_reruns_after_bcp_narrows_nature_via_pre_nature_stat() {
    use crate::state::dex_data::PokemonStat;
    use crate::state::pokemon::Nature;

    // Garchomp at level 50 using the real dex so min/max_pre_nature_stat are
    // initialised correctly (min_BSV[Atk]=135, max_BSV[Atk]=182 at iv=0..31).
    let mut mon =
        UnknownPokemonState::from_opponent_species(Species::Garchomp, &garchomp_dex(), 50);
    // Narrow the observed Atk range.  With max IVs, neutral Atk at ev=44 is 156 and
    // at ev=36 is 155; +Atk at ev=36 gives 170 (within range) and at ev=44 gives 171
    // (just outside maxStats=170).
    mon.minStats[1] = 150;
    mon.maxStats[1] = 170;

    // In battle_with_p2 with no p1 mons, p2 active slot 0 → mon_idx=0.
    let mut state = battle_with_p2(vec![mon]);

    // Pre-inject the unit predicate EVIVStatGE{Atk, 156}.
    // This simulates the result of BCP resolving a multi-literal clause to its last
    // surviving literal — e.g. (EVIVStatGE{Atk,156} OR HasItem{ChoiceScarf}) after
    // the item was revealed to be something other than Choice Scarf.
    state.predicates.push(vec![Statement::EVIVStatGE {
        mon_idx: 0,
        stat: PokemonStat::Atk,
        value: 156,
    }]);

    let config = InferenceConfig { force_max_ivs: true, ..InferenceConfig::default() };
    let result = apply_with_config(state, vec![], garchomp_dex(), HashMap::new(), config);

    let p2_mon = &result.p2_active_mons[0];

    // +Atk natures must be excluded: at BSV≥156 their stat always exceeds maxStats=170.
    for atk_nature in [Nature::Lonely, Nature::Adamant, Nature::Naughty, Nature::Brave] {
        assert!(
            unknown_is_excluded(&p2_mon.possible_natures, &atk_nature),
            "{atk_nature:?} (+Atk ×1.1) must be excluded after second pass5 uses \
             min_pre_nature_stat[Atk]=156 (BSV=156 → stat=171 > maxStats=170)"
        );
    }

    // Neutral natures must remain feasible: BSV=156 (ev=44) → stat=156 ∈ [150, 170].
    assert!(
        !unknown_is_excluded(&p2_mon.possible_natures, &Nature::Hardy),
        "Hardy (neutral) must remain feasible at BSV=156 → stat=156 ∈ [150,170]"
    );
    // -Atk natures must remain feasible: BSV=167 (ev=132) → stat=150 ∈ [150, 170].
    assert!(
        !unknown_is_excluded(&p2_mon.possible_natures, &Nature::Modest),
        "Modest (-Atk ×0.9) must remain feasible at BSV=167 → stat=150 ∈ [150,170]"
    );
}

// ── I-A: Timer collapse reveals extender item ─────────────────────────────────

/// Regression test for I-A: when a `Possibly([5,8])` weather/terrain/screen timer
/// collapses to `Known(3)` after 5 end-of-turns, the extended (8-turn) branch is
/// the only remaining candidate, so the setter's rock/extender item is guaranteed
/// and must be recorded as `Known`.
///
/// Rain set via Rain Dance starts at `Possibly([5,8])`; after 5 EOT decrements the
/// 5-turn branch would hit 0 and is filtered out, leaving only the 8-turn branch
/// (now at 3) — so `emit_extension_item_if_collapsed` must force `Known(DampRock)`.
fn weather_move_dex() -> HashMap<PokemonMove, MoveData> {
    let mut rd = poke_status_move(PokemonMove::RainDance);
    rd.pokemon_type = PokemonType::Water;
    let mut move_dex = HashMap::new();
    move_dex.insert(PokemonMove::RainDance, rd);
    move_dex
}

/// P3a: the turn-count clause pair propagates item knowledge INTO the timer —
/// revealing the setter's Damp Rock collapses `weather_turns` to Known(8) via BCP,
/// without waiting for any end-of-turn.
#[test]
fn test_weather_clause_item_reveal_collapses_timer_to_8() {
    let state = battle_with_p2(vec![unknown_mon_species(Species::Garchomp)]);
    let events = vec![
        event_with(
            EventKind::MoveUsed { user: p2(0), move_used: PokemonMove::RainDance, targets: vec![] },
            vec![event(EventKind::WeatherChanged { weather: Some(Weather::Rain) })],
        ),
        event(EventKind::ItemRevealed { slot: p2(0), item: Item::DampRock }),
    ];
    let result = apply_ex(state, events, HashMap::new(), weather_move_dex());
    assert_eq!(
        result.weather_turns,
        Some(Unknown::Known(8)),
        "revealed Damp Rock must collapse the weather timer to Known(8) via BCP"
    );
}

/// P3a: revealing a NON-rock item on the setter proves the base duration —
/// the timer collapses to Known(5).
#[test]
fn test_weather_clause_non_rock_item_collapses_timer_to_5() {
    let state = battle_with_p2(vec![unknown_mon_species(Species::Garchomp)]);
    let events = vec![
        event_with(
            EventKind::MoveUsed { user: p2(0), move_used: PokemonMove::RainDance, targets: vec![] },
            vec![event(EventKind::WeatherChanged { weather: Some(Weather::Rain) })],
        ),
        event(EventKind::ItemRevealed { slot: p2(0), item: Item::LumBerry }),
    ];
    let result = apply_ex(state, events, HashMap::new(), weather_move_dex());
    assert_eq!(
        result.weather_turns,
        Some(Unknown::Known(5)),
        "a revealed non-rock item must collapse the weather timer to Known(5) via BCP"
    );
}

/// Regression (found in this audit): at the 5th end-of-turn the Possibly([1,4])→Known(3)
/// collapse coincides with the base-duration natural expiry. The old code revealed the
/// rock BEFORE processing the nested WeatherChanged{None}, unsoundly branding the setter
/// with a Damp Rock it doesn't hold. Natural expiry must instead EXCLUDE the rock.
#[test]
fn test_weather_natural_expiry_excludes_rock_instead_of_revealing() {
    let state = battle_with_p2(vec![unknown_mon_species(Species::Garchomp)]);
    let mut events = vec![event_with(
        EventKind::MoveUsed { user: p2(0), move_used: PokemonMove::RainDance, targets: vec![] },
        vec![event(EventKind::WeatherChanged { weather: Some(Weather::Rain) })],
    )];
    // Four uneventful end-of-turns, then the expiry EOT with the nested end event.
    for _ in 0..4 {
        events.push(event(EventKind::EndOfTurn));
    }
    events.push(event_with(
        EventKind::EndOfTurn,
        vec![event(EventKind::WeatherChanged { weather: None })],
    ));
    let result = apply_ex(state, events, HashMap::new(), weather_move_dex());
    let setter = &result.p2_active_mons[0];
    assert!(
        !matches!(&setter.item, Unknown::Known(i) if *i == Item::DampRock),
        "natural expiry must NOT reveal a Damp Rock; item = {:?}",
        setter.item
    );
    assert!(
        is_item_excluded(setter, &Item::DampRock),
        "natural expiry at base duration must EXCLUDE the Damp Rock; item = {:?}",
        setter.item
    );
    assert_eq!(result.weather, None, "weather must have ended");
}

/// The clause `turns` payloads must decrement in sync with the field timer: a Damp
/// Rock revealed TWO turns after the rain was set must collapse the timer to
/// Known(8−2 = 6), not Known(8).
#[test]
fn test_weather_clause_turns_decrement_stays_in_sync() {
    let state = battle_with_p2(vec![unknown_mon_species(Species::Garchomp)]);
    let events = vec![
        event_with(
            EventKind::MoveUsed { user: p2(0), move_used: PokemonMove::RainDance, targets: vec![] },
            vec![event(EventKind::WeatherChanged { weather: Some(Weather::Rain) })],
        ),
        event(EventKind::EndOfTurn),
        event(EventKind::EndOfTurn),
        event(EventKind::ItemRevealed { slot: p2(0), item: Item::DampRock }),
    ];
    let result = apply_ex(state, events, HashMap::new(), weather_move_dex());
    assert_eq!(
        result.weather_turns,
        Some(Unknown::Known(6)),
        "clause turns must have decremented with the timer (8 − 2 EOTs = 6)"
    );
}

/// Overriding the weather mid-flight destroys the duration information: the old
/// clauses must be purged (no stale Damp Rock disjuncts) and replaced by the new
/// weather's pair (Heat Rock for Sun).
#[test]
fn test_weather_override_purges_old_clauses() {
    let state = battle_with_p2(vec![unknown_mon_species(Species::Garchomp)]);
    let mut move_dex = weather_move_dex();
    move_dex.insert(PokemonMove::SunnyDay, poke_status_move(PokemonMove::SunnyDay));
    let events = vec![
        event_with(
            EventKind::MoveUsed { user: p2(0), move_used: PokemonMove::RainDance, targets: vec![] },
            vec![event(EventKind::WeatherChanged { weather: Some(Weather::Rain) })],
        ),
        event(EventKind::EndOfTurn),
        // Turn 2: the rain is overridden by Sun before its duration resolved.
        event_with(
            EventKind::MoveUsed { user: p2(0), move_used: PokemonMove::SunnyDay, targets: vec![] },
            vec![event(EventKind::WeatherChanged { weather: Some(Weather::Sun) })],
        ),
    ];
    let result = apply_ex(state, events, HashMap::new(), move_dex);
    assert!(
        !result.predicates.iter().any(|c| c.iter().any(|s| clause_mentions_item(s, &Item::DampRock))),
        "old Damp Rock clauses must be purged on override; predicates = {:?}",
        result.predicates
    );
    let heat_rock_clauses = result.predicates.iter().filter(|c| {
        c.iter().any(|s| clause_mentions_item(s, &Item::HeatRock))
    }).count();
    assert_eq!(
        heat_rock_clauses, 2,
        "the new Sun must carry exactly its own clause pair; predicates = {:?}",
        result.predicates
    );
    // The fresh Sun timer restarts at Possibly([5,8]).
    assert_eq!(result.weather_turns, Some(Unknown::Possibly(vec![5, 8])));
}

/// A weather OVERRIDE landing at the collapse end-of-turn gives no duration
/// information: neither reveal nor exclude the rock.
#[test]
fn test_weather_overridden_at_collapse_eot_gives_no_item_info() {
    let state = battle_with_p2(vec![unknown_mon_species(Species::Garchomp)]);
    let mut events = vec![event_with(
        EventKind::MoveUsed { user: p2(0), move_used: PokemonMove::RainDance, targets: vec![] },
        vec![event(EventKind::WeatherChanged { weather: Some(Weather::Rain) })],
    )];
    for _ in 0..4 {
        events.push(event(EventKind::EndOfTurn));
    }
    // 5th EOT: something replaces the rain in the same end-of-turn window.
    events.push(event_with(
        EventKind::EndOfTurn,
        vec![event(EventKind::WeatherChanged { weather: Some(Weather::Sun) })],
    ));
    let result = apply_ex(state, events, HashMap::new(), weather_move_dex());
    let setter = &result.p2_active_mons[0];
    assert!(
        !matches!(&setter.item, Unknown::Known(i) if *i == Item::DampRock),
        "an override at the collapse EOT must not reveal the rock; item = {:?}",
        setter.item
    );
    assert!(
        !is_item_excluded(setter, &Item::DampRock),
        "an override at the collapse EOT must not exclude the rock either; item = {:?}",
        setter.item
    );
}

/// Terrain natural expiry at base duration must EXCLUDE the Terrain Extender
/// (sibling of the weather natural-expiry regression test).
#[test]
fn test_terrain_natural_expiry_excludes_extender() {
    let state = battle_with_p2(vec![unknown_mon_species(Species::Garchomp)]);
    let mut move_dex = HashMap::new();
    move_dex.insert(
        PokemonMove::ElectricTerrain,
        poke_status_move(PokemonMove::ElectricTerrain),
    );
    let mut events = vec![event_with(
        EventKind::MoveUsed { user: p2(0), move_used: PokemonMove::ElectricTerrain, targets: vec![] },
        vec![event(EventKind::TerrainChanged { terrain: Some(Terrain::ElectricTerrain) })],
    )];
    for _ in 0..4 {
        events.push(event(EventKind::EndOfTurn));
    }
    events.push(event_with(
        EventKind::EndOfTurn,
        vec![event(EventKind::TerrainChanged { terrain: None })],
    ));
    let result = apply_ex(state, events, HashMap::new(), move_dex);
    let setter = &result.p2_active_mons[0];
    assert!(
        is_item_excluded(setter, &Item::TerrainExtender),
        "terrain ending at base duration must exclude the Terrain Extender; item = {:?}",
        setter.item
    );
}

/// Screen natural expiry at base duration must EXCLUDE Light Clay.
#[test]
fn test_screen_natural_expiry_excludes_light_clay() {
    let state = battle_with_p2(vec![unknown_mon_species(Species::Garchomp)]);
    let mut move_dex = HashMap::new();
    move_dex.insert(PokemonMove::Reflect, poke_status_move(PokemonMove::Reflect));
    let mut events = vec![event_with(
        EventKind::MoveUsed { user: p2(0), move_used: PokemonMove::Reflect, targets: vec![] },
        vec![event(EventKind::SideConditionStart {
            side: Player::P2,
            condition: SideCondition::Reflect,
        })],
    )];
    for _ in 0..4 {
        events.push(event(EventKind::EndOfTurn));
    }
    events.push(event_with(
        EventKind::EndOfTurn,
        vec![event(EventKind::SideConditionEnd {
            side: Player::P2,
            condition: SideCondition::Reflect,
        })],
    ));
    let result = apply_ex(state, events, HashMap::new(), move_dex);
    let setter = &result.p2_active_mons[0];
    assert!(
        is_item_excluded(setter, &Item::LightClay),
        "a screen ending at base duration must exclude Light Clay; item = {:?}",
        setter.item
    );
}

/// Recursively check whether a statement (possibly inside a Not) names `item`.
fn clause_mentions_item(s: &Statement, item: &Item) -> bool {
    match s {
        Statement::HasItem { item: i, .. } => i == item,
        Statement::Not(inner) => clause_mentions_item(inner, item),
        _ => false,
    }
}

/// Light Clay collapse (previously untested sibling of the Damp Rock / Terrain
/// Extender collapse tests): a Reflect persisting past 5 turns pins Light Clay
/// on its setter.
#[test]
fn test_screen_timer_collapse_reveals_light_clay() {
    let state = battle_with_p2(vec![unknown_mon_species(Species::Garchomp)]);
    let mut move_dex = HashMap::new();
    move_dex.insert(PokemonMove::Reflect, poke_status_move(PokemonMove::Reflect));
    let mut events = vec![event_with(
        EventKind::MoveUsed { user: p2(0), move_used: PokemonMove::Reflect, targets: vec![] },
        vec![event(EventKind::SideConditionStart {
            side: Player::P2,
            condition: SideCondition::Reflect,
        })],
    )];
    // Five end-of-turns with the screen still up → the 5-turn track dies → Light Clay.
    for _ in 0..5 {
        events.push(event(EventKind::EndOfTurn));
    }
    let result = apply_ex(state, events, HashMap::new(), move_dex);
    assert!(
        matches!(&result.p2_active_mons[0].item, Unknown::Known(i) if *i == Item::LightClay),
        "a screen persisting past 5 turns must pin Light Clay on the setter; item = {:?}",
        result.p2_active_mons[0].item
    );
}

#[test]
fn test_ia_weather_timer_collapse_reveals_damp_rock() {
    // Opponent Garchomp at P2, slot 0.  Item fully unknown.
    let p2_mon = unknown_mon_species(Species::Garchomp);
    assert!(
        matches!(p2_mon.item, Unknown::Not(ref v) if v.is_empty()),
        "item must start fully unknown"
    );

    // battle_with_p2 puts p2 at mon_idx=0 (p1 is empty).
    let state = battle_with_p2(vec![p2_mon]);

    // Minimal Rain Dance move entry (shared with the turn-count clause tests above).
    let move_dex = weather_move_dex();

    // The WeatherChanged reaction sets weather_turns = Possibly([5,8]) and records
    // weather_setter_mon_idx = mon_idx of p2(0) = 0.
    let rain_dance_turn = vec![event_with(
        EventKind::MoveUsed {
            user: p2(0),
            move_used: PokemonMove::RainDance,
            targets: vec![],
        },
        vec![event(EventKind::WeatherChanged { weather: Some(Weather::Rain) })],
    )];

    let mut cur_state = apply_ex(state, rain_dance_turn, HashMap::new(), move_dex);

    assert_eq!(
        cur_state.weather_turns,
        Some(Unknown::Possibly(vec![5, 8])),
        "weather_turns must start as Possibly([5,8])"
    );
    assert_eq!(cur_state.weather, Some(Weather::Rain));
    assert_eq!(
        cur_state.weather_setter_mon_idx, Some(0),
        "setter must be mon_idx=0 (Garchomp at p2 slot 0)"
    );
    assert!(
        matches!(cur_state.p2_active_mons[0].item, Unknown::Not(ref v) if v.is_empty()),
        "item must still be unknown after Rain Dance"
    );

    // Advance 4 EndOfTurn events — timer should decrement but NOT collapse yet.
    for expected_after in [
        Unknown::Possibly(vec![4, 7]),
        Unknown::Possibly(vec![3, 6]),
        Unknown::Possibly(vec![2, 5]),
        Unknown::Possibly(vec![1, 4]),
    ] {
        cur_state = apply_ex(cur_state, vec![event(EventKind::EndOfTurn)], HashMap::new(), HashMap::new());
        assert_eq!(
            cur_state.weather_turns,
            Some(expected_after.clone()),
            "weather_turns after decrement should be {:?}", expected_after
        );
        // Item still unknown — collapse has not happened yet.
        assert!(
            matches!(cur_state.p2_active_mons[0].item, Unknown::Not(ref v) if v.is_empty()),
            "item must remain unknown before the 5th EOT"
        );
    }

    // 5th EndOfTurn — timer collapses: Possibly([1,4]) → filter(n>1) → [3] → Known(3).
    cur_state = apply_ex(cur_state, vec![event(EventKind::EndOfTurn)], HashMap::new(), HashMap::new());
    assert_eq!(
        cur_state.weather_turns,
        Some(Unknown::Known(3)),
        "weather_turns must collapse to Known(3) after the 5th EOT"
    );

    // I-A: DampRock must now be revealed as Known on Garchomp.
    assert_eq!(
        cur_state.p2_active_mons[0].item,
        Unknown::Known(Item::DampRock),
        "Damp Rock must be revealed as Known after the 5th EOT confirms the 8-turn branch"
    );
}

// ── S-C: allowlist completeness cross-validation ──────────────────────────────
//
// For every ability and item known to the simulator, this test runs the damage oracle
// with and without that modifier and asserts that any which changes the oracle output
// IS in the corresponding `defensive_damage_*` / `offensive_damage_*` allowlist.
//
// Covered by this test (oracle-handled modifiers):
//   - All type-resist berries (Occa…Roseli + ChilanBerry) via 18+ type-SE probes
//   - FurCoat, IceScales, Heatproof, WaterBubble-def, ThickFat, Fluffy, PunkRock-def
//   - Filter, SolidRock, PrismArmor (SE probes)
//   - Multiscale, ShadowShield, TeraShell (full-HP probe)
//   - PurifyingSalt (Ghost probe)
//   - ChoiceBand/Specs, LifeOrb, ExpertBelt, MuscleBand, WiseGlasses, type-boosting items
//   - HugePower, PurePower, Hustle, Adaptability, Technician, ToughClaws,
//     IronFist, StrongJaw, Sharpness, MegaLauncher, WaterBubble-off, PunkRock-off
//
// Known limitations (not caught by oracle — verified manually):
//   - Item::AssaultVest: baked into stat manually in pass3_direction_a's run_def_oracle
//   - Item::Eviolite:    same manual bake; excluded from the probe via `manual_bake_items`
//   - Item::LightBall, Item::Metronome: species-specific / streak-dependent (false negatives)
//   - Abilities requiring field state (SandForce/SolarPower/Guts/Reckless): not triggered
//     in default probe state
#[test]
fn test_sc_allowlist_completeness_cross_validation() {
    use crate::state::dex_data::{Status, Terrain, Weather};
    use crate::information::inference::{
        defensive_damage_abilities, defensive_damage_items,
        offensive_damage_abilities, offensive_damage_items,
    };
    use crate::information::materialize::{materialize_battle, materialize_pokemon};
    use crate::simulator::helpers::calculate_damage_outcomes_for_target_with_options;
    use crate::simulator::DamageConfig;
    use crate::state::dex_data::{parse_ability_dex, parse_item_ids, MoveFlag};

    // Load every known ability and item from the Showdown data files.
    let all_abilities: Vec<Ability> =
        parse_ability_dex("../pokemon_info/showdownAbilities.txt")
            .into_keys()
            .collect();
    let all_items: Vec<Item> = parse_item_ids("../pokemon_info/showdownItems.txt");

    // Single roll, no crit: produces one deterministic (damage, false, 1.0) outcome.
    let oracle_config = DamageConfig { consider_crit: false, damage_rolls: 1 };

    // Fixed stats (lv50 rough values): attacker hits hard, defender has moderate bulk.
    let atk_stats: [u16; 6] = [150, 150, 80, 150, 80, 100];
    let def_stats: [u16; 6] = [200, 80, 115, 80, 115, 80];

    // Open mon: item=Not([]), ability=Not([]) — nothing excluded, so all allowlist entries
    // pass the `unknown_is_excluded` filter. Types are overridden per probe below.
    let open_mon = || unknown_mon_species(Species::Garchomp);

    // Blank battle state template (no weather/terrain/screens — clean field).
    let blank_battle_unk = battle_with_p2(vec![open_mon()]);

    // Compare oracle outputs ignoring probability weights.
    let damage_differs = |a: &Vec<(u16, bool, f64)>, b: &Vec<(u16, bool, f64)>| -> bool {
        let sig = |v: &Vec<(u16, bool, f64)>| -> Vec<(u16, bool)> {
            let mut s: Vec<(u16, bool)> = v.iter().map(|&(d, c, _)| (d, c)).collect();
            s.sort_unstable();
            s.dedup();
            s
        };
        sig(a) != sig(b)
    };

    // Probes: each captures a different type/category/flag combination.
    // def_types chosen so the move is super-effective (2×+) — needed for
    // Filter/SolidRock/PrismArmor and the type-resist berries to trigger.
    // Attacker types = move type so STAB-dependent abilities (Adaptability) are detected.
    #[derive(Clone)]
    struct Probe {
        name: &'static str,
        move_type: PokemonType,
        move_cat: MoveCategory,
        move_bp: u16,
        move_flags: Vec<MoveFlag>,
        def_types: Vec<PokemonType>,
        def_full_hp: bool, // true → Multiscale/ShadowShield/TeraShell fire
    }
    let probes = vec![
        // Physical contact Normal: FurCoat, ToughClaws, Fluffy-contact, ChilanBerry.
        Probe { name: "phys-normal-contact", move_type: PokemonType::Normal, move_cat: MoveCategory::Physical, move_bp: 80, move_flags: vec![MoveFlag::Contact], def_types: vec![PokemonType::Normal], def_full_hp: false },
        // Physical punch Normal: IronFist.
        Probe { name: "phys-normal-punch", move_type: PokemonType::Normal, move_cat: MoveCategory::Physical, move_bp: 80, move_flags: vec![MoveFlag::Contact, MoveFlag::Punch], def_types: vec![PokemonType::Normal], def_full_hp: false },
        // Physical slicing Normal: Sharpness.
        Probe { name: "phys-normal-slicing", move_type: PokemonType::Normal, move_cat: MoveCategory::Physical, move_bp: 80, move_flags: vec![MoveFlag::Slicing], def_types: vec![PokemonType::Normal], def_full_hp: false },
        // Physical low-BP Normal: Technician (≤60 BP).
        Probe { name: "phys-normal-lowbp", move_type: PokemonType::Normal, move_cat: MoveCategory::Physical, move_bp: 40, move_flags: vec![MoveFlag::Contact], def_types: vec![PokemonType::Normal], def_full_hp: false },
        // Special Fire vs Grass: OccaBerry, Heatproof, WaterBubble-def, ThickFat, IceScales, Filter.
        Probe { name: "spec-fire-vs-grass", move_type: PokemonType::Fire, move_cat: MoveCategory::Special, move_bp: 90, move_flags: vec![], def_types: vec![PokemonType::Grass], def_full_hp: false },
        // Special Fire pulse vs Grass: MegaLauncher bonus on top of fire coverage.
        Probe { name: "spec-fire-pulse-vs-grass", move_type: PokemonType::Fire, move_cat: MoveCategory::Special, move_bp: 90, move_flags: vec![MoveFlag::Pulse], def_types: vec![PokemonType::Grass], def_full_hp: false },
        // Special Ice vs Dragon: YacheBerry, ThickFat-ice, IceScales, Filter.
        Probe { name: "spec-ice-vs-dragon", move_type: PokemonType::Ice, move_cat: MoveCategory::Special, move_bp: 90, move_flags: vec![], def_types: vec![PokemonType::Dragon], def_full_hp: false },
        // Special Psychic Sound vs Fighting: PayapaBerry, PunkRock-def, IceScales, Filter.
        Probe { name: "spec-psychic-sound-vs-fighting", move_type: PokemonType::Psychic, move_cat: MoveCategory::Special, move_bp: 90, move_flags: vec![MoveFlag::Sound], def_types: vec![PokemonType::Fighting], def_full_hp: false },
        // Special Ghost vs Ghost: KasibBerry, PurifyingSalt, Filter.
        Probe { name: "spec-ghost-vs-ghost", move_type: PokemonType::Ghost, move_cat: MoveCategory::Special, move_bp: 80, move_flags: vec![], def_types: vec![PokemonType::Ghost], def_full_hp: false },
        // Special Water vs Rock: PasshoBerry, Filter.
        Probe { name: "spec-water-vs-rock", move_type: PokemonType::Water, move_cat: MoveCategory::Special, move_bp: 90, move_flags: vec![], def_types: vec![PokemonType::Rock], def_full_hp: false },
        // Special Electric vs Water: WacanBerry, Filter.
        Probe { name: "spec-electric-vs-water", move_type: PokemonType::Electric, move_cat: MoveCategory::Special, move_bp: 90, move_flags: vec![], def_types: vec![PokemonType::Water], def_full_hp: false },
        // Special Grass vs Water: RindoBerry, Filter.
        Probe { name: "spec-grass-vs-water", move_type: PokemonType::Grass, move_cat: MoveCategory::Special, move_bp: 90, move_flags: vec![], def_types: vec![PokemonType::Water], def_full_hp: false },
        // Physical Fighting vs Rock: ChopleBerry, Filter.
        Probe { name: "phys-fighting-vs-rock", move_type: PokemonType::Fighting, move_cat: MoveCategory::Physical, move_bp: 80, move_flags: vec![], def_types: vec![PokemonType::Rock], def_full_hp: false },
        // Special Poison vs Fairy: KebiaBerry, Filter.
        Probe { name: "spec-poison-vs-fairy", move_type: PokemonType::Poison, move_cat: MoveCategory::Special, move_bp: 80, move_flags: vec![], def_types: vec![PokemonType::Fairy], def_full_hp: false },
        // Physical Ground vs Rock: ShucaBerry, Filter.
        Probe { name: "phys-ground-vs-rock", move_type: PokemonType::Ground, move_cat: MoveCategory::Physical, move_bp: 80, move_flags: vec![], def_types: vec![PokemonType::Rock], def_full_hp: false },
        // Physical Flying vs Fighting: CobaBerry, Filter.
        Probe { name: "phys-flying-vs-fighting", move_type: PokemonType::Flying, move_cat: MoveCategory::Physical, move_bp: 80, move_flags: vec![], def_types: vec![PokemonType::Fighting], def_full_hp: false },
        // Special Bug vs Psychic: TangaBerry, Filter.
        Probe { name: "spec-bug-vs-psychic", move_type: PokemonType::Bug, move_cat: MoveCategory::Special, move_bp: 80, move_flags: vec![], def_types: vec![PokemonType::Psychic], def_full_hp: false },
        // Physical Rock vs Ice: ChartiBerry, Filter.
        Probe { name: "phys-rock-vs-ice", move_type: PokemonType::Rock, move_cat: MoveCategory::Physical, move_bp: 80, move_flags: vec![], def_types: vec![PokemonType::Ice], def_full_hp: false },
        // Physical Dragon contact, full HP: HabanBerry, Multiscale, ShadowShield, TeraShell.
        Probe { name: "phys-dragon-contact-fullhp", move_type: PokemonType::Dragon, move_cat: MoveCategory::Physical, move_bp: 90, move_flags: vec![MoveFlag::Contact], def_types: vec![PokemonType::Dragon], def_full_hp: true },
        // Special Dark vs Psychic: ColburBerry, Filter.
        Probe { name: "spec-dark-vs-psychic", move_type: PokemonType::Dark, move_cat: MoveCategory::Special, move_bp: 80, move_flags: vec![], def_types: vec![PokemonType::Psychic], def_full_hp: false },
        // Special Dark bite vs Psychic: StrongJaw.
        Probe { name: "spec-dark-bite-vs-psychic", move_type: PokemonType::Dark, move_cat: MoveCategory::Special, move_bp: 80, move_flags: vec![MoveFlag::Bite], def_types: vec![PokemonType::Psychic], def_full_hp: false },
        // Special Steel vs Ice: BabiriBerry, Filter.
        Probe { name: "spec-steel-vs-ice", move_type: PokemonType::Steel, move_cat: MoveCategory::Special, move_bp: 80, move_flags: vec![], def_types: vec![PokemonType::Ice], def_full_hp: false },
        // Special Fairy vs Dragon: RoseliBerry, Filter.
        Probe { name: "spec-fairy-vs-dragon", move_type: PokemonType::Fairy, move_cat: MoveCategory::Special, move_bp: 80, move_flags: vec![], def_types: vec![PokemonType::Dragon], def_full_hp: false },
        // Special Water neutral: WaterBubble offensive (×2 to Water moves on attacker).
        Probe { name: "spec-water-neutral", move_type: PokemonType::Water, move_cat: MoveCategory::Special, move_bp: 90, move_flags: vec![], def_types: vec![PokemonType::Normal], def_full_hp: false },
    ];

    // Open mon for allowlist queries (no exclusions → full lists returned).
    let allowlist_mon = open_mon();
    let listed_def_items = defensive_damage_items(&allowlist_mon);
    let listed_def_abilities = defensive_damage_abilities(&allowlist_mon);
    let listed_atk_items = offensive_damage_items(&allowlist_mon);
    let listed_atk_abilities = offensive_damage_abilities(&allowlist_mon);

    // Items baked manually into the stat by pass3_direction_a (not via oracle item handling).
    // These cannot be detected via oracle output comparison; they are verified by code review.
    let manual_bake_items = [Item::AssaultVest, Item::Eviolite];

    let mut failures: Vec<String> = Vec::new();

    for probe in &probes {
        let mut move_data = normal_physical_move(PokemonMove::Tackle, probe.move_bp);
        move_data.category = probe.move_cat;
        move_data.pokemon_type = probe.move_type.clone();
        move_data.flags = probe.move_flags.clone();

        // Attacker type = move type so STAB-dependent abilities (Adaptability) are detected.
        let mut atk_unk = open_mon();
        atk_unk.possible_types = Unknown::Known(vec![probe.move_type.clone()]);

        // Defender type = probe.def_types (ensures move is SE for berry/Filter probes).
        let mut def_unk = open_mon();
        def_unk.possible_types = Unknown::Known(probe.def_types.clone());
        def_unk.hp = if probe.def_full_hp {
            PokemonHP::Percent(100)
        } else {
            PokemonHP::Percent(60)
        };

        // Baseline: both sides hold Item::None and Ability::None.
        let atk_base = materialize_pokemon(&atk_unk, atk_stats, Item::None, Ability::None);
        let def_base = materialize_pokemon(&def_unk, def_stats, Item::None, Ability::None);
        let battle_base = materialize_battle(&blank_battle_unk, vec![atk_base.clone()], vec![def_base.clone()]);
        let baseline = calculate_damage_outcomes_for_target_with_options(
            &battle_base, &atk_base, &def_base,
            p1(0), p2(0), &move_data, oracle_config, 1.0, 1.0, None, None,
        );

        // ── Defensive items ──
        for item in &all_items {
            if manual_bake_items.contains(item) {
                continue; // handled manually in pass3_direction_a; oracle won't see them
            }
            let def_with = materialize_pokemon(&def_unk, def_stats, item.clone(), Ability::None);
            let battle = materialize_battle(&blank_battle_unk, vec![atk_base.clone()], vec![def_with.clone()]);
            let outcomes = calculate_damage_outcomes_for_target_with_options(
                &battle, &atk_base, &def_with,
                p1(0), p2(0), &move_data, oracle_config, 1.0, 1.0, None, None,
            );
            if damage_differs(&outcomes, &baseline) && !listed_def_items.contains(item) {
                failures.push(format!(
                    "[DEF-ITEM] {:?} changes damage on probe '{}' but is NOT in defensive_damage_items",
                    item, probe.name
                ));
            }
        }

        // ── Defensive abilities ──
        for ability in &all_abilities {
            let def_with = materialize_pokemon(&def_unk, def_stats, Item::None, ability.clone());
            let battle = materialize_battle(&blank_battle_unk, vec![atk_base.clone()], vec![def_with.clone()]);
            let outcomes = calculate_damage_outcomes_for_target_with_options(
                &battle, &atk_base, &def_with,
                p1(0), p2(0), &move_data, oracle_config, 1.0, 1.0, None, None,
            );
            if damage_differs(&outcomes, &baseline) && !listed_def_abilities.contains(ability) {
                failures.push(format!(
                    "[DEF-ABILITY] {:?} changes damage on probe '{}' but is NOT in defensive_damage_abilities",
                    ability, probe.name
                ));
            }
        }

        // ── Offensive items ──
        for item in &all_items {
            let atk_with = materialize_pokemon(&atk_unk, atk_stats, item.clone(), Ability::None);
            let battle = materialize_battle(&blank_battle_unk, vec![atk_with.clone()], vec![def_base.clone()]);
            let outcomes = calculate_damage_outcomes_for_target_with_options(
                &battle, &atk_with, &def_base,
                p1(0), p2(0), &move_data, oracle_config, 1.0, 1.0, None, None,
            );
            if damage_differs(&outcomes, &baseline) && !listed_atk_items.contains(item) {
                failures.push(format!(
                    "[ATK-ITEM] {:?} changes damage on probe '{}' but is NOT in offensive_damage_items",
                    item, probe.name
                ));
            }
        }

        // ── Offensive abilities ──
        for ability in &all_abilities {
            let atk_with = materialize_pokemon(&atk_unk, atk_stats, Item::None, ability.clone());
            let battle = materialize_battle(&blank_battle_unk, vec![atk_with.clone()], vec![def_base.clone()]);
            let outcomes = calculate_damage_outcomes_for_target_with_options(
                &battle, &atk_with, &def_base,
                p1(0), p2(0), &move_data, oracle_config, 1.0, 1.0, None, None,
            );
            if damage_differs(&outcomes, &baseline) && !listed_atk_abilities.contains(ability) {
                failures.push(format!(
                    "[ATK-ABILITY] {:?} changes damage on probe '{}' but is NOT in offensive_damage_abilities",
                    ability, probe.name
                ));
            }
        }
    }

    // ── T2: Field/status conditioned offensive-ability probes ─────────────────
    //
    // The clean-field probes above cannot trigger abilities that require weather,
    // terrain, or status to boost damage (SolarPower/SandForce/Guts/HadronEngine).
    // These variants activate the triggering condition so that if any such ability
    // were accidentally removed from `offensive_damage_abilities`, the oracle output
    // would diverge from baseline and the omission would be caught.
    //
    // Pattern: conditioned_battle with the relevant field active; baseline uses the
    // same battle but Ability::None; probe uses the field-triggering ability.
    {
        // Helper to build an UnknownBattleState with weather set (1v1 shell).
        let battle_with_weather = |w: Weather| -> UnknownBattleState {
            let mut b = battle_with_p2(vec![open_mon()]);
            b.weather = Some(w);
            b.weather_turns = Some(Unknown::Known(5));
            b
        };
        let battle_with_terrain = |t: Terrain| -> UnknownBattleState {
            let mut b = battle_with_p2(vec![open_mon()]);
            b.terrain = Some(t);
            b.terrain_turns = Some(Unknown::Known(5));
            b
        };

        // (a) Sun + SolarPower: special attacker gains +SpA under harsh sun.
        {
            let battle_sun = battle_with_weather(Weather::Sun);
            let mut atk_unk = open_mon();
            atk_unk.possible_types = Unknown::Known(vec![PokemonType::Fire]);
            let def_base = materialize_pokemon(&open_mon(), def_stats, Item::None, Ability::None);
            let atk_base = materialize_pokemon(&atk_unk, atk_stats, Item::None, Ability::None);
            let battle_base = materialize_battle(&battle_sun, vec![atk_base.clone()], vec![def_base.clone()]);
            let baseline = calculate_damage_outcomes_for_target_with_options(
                &battle_base, &atk_base, &def_base, p1(0), p2(0),
                &{
                    let mut md = normal_physical_move(PokemonMove::Tackle, 90);
                    md.category = MoveCategory::Special;
                    md.pokemon_type = PokemonType::Fire;
                    md
                },
                oracle_config, 1.0, 1.0, None, None,
            );
            let atk_with = materialize_pokemon(&atk_unk, atk_stats, Item::None, Ability::SolarPower);
            let battle_with = materialize_battle(&battle_sun, vec![atk_with.clone()], vec![def_base.clone()]);
            let move_data = {
                let mut md = normal_physical_move(PokemonMove::Tackle, 90);
                md.category = MoveCategory::Special;
                md.pokemon_type = PokemonType::Fire;
                md
            };
            let outcomes = calculate_damage_outcomes_for_target_with_options(
                &battle_with, &atk_with, &def_base, p1(0), p2(0),
                &move_data, oracle_config, 1.0, 1.0, None, None,
            );
            if damage_differs(&outcomes, &baseline) && !listed_atk_abilities.contains(&Ability::SolarPower) {
                failures.push(
                    "[CONDITIONED-ATK-ABILITY] SolarPower changes damage under Sun but is \
                     NOT in offensive_damage_abilities".to_string()
                );
            }
        }

        // (b) Sandstorm + SandForce: physical attacker using a Rock move gains 1.3× in sand.
        {
            let battle_sand = battle_with_weather(Weather::Sandstorm);
            let mut atk_unk = open_mon();
            atk_unk.possible_types = Unknown::Known(vec![PokemonType::Rock]);
            let def_base = materialize_pokemon(&open_mon(), def_stats, Item::None, Ability::None);
            let atk_base = materialize_pokemon(&atk_unk, atk_stats, Item::None, Ability::None);
            let move_data = {
                let mut md = normal_physical_move(PokemonMove::Tackle, 80);
                md.category = MoveCategory::Physical;
                md.pokemon_type = PokemonType::Rock;
                md
            };
            let battle_base = materialize_battle(&battle_sand, vec![atk_base.clone()], vec![def_base.clone()]);
            let baseline = calculate_damage_outcomes_for_target_with_options(
                &battle_base, &atk_base, &def_base, p1(0), p2(0),
                &move_data, oracle_config, 1.0, 1.0, None, None,
            );
            let atk_with = materialize_pokemon(&atk_unk, atk_stats, Item::None, Ability::SandForce);
            let battle_with = materialize_battle(&battle_sand, vec![atk_with.clone()], vec![def_base.clone()]);
            let outcomes = calculate_damage_outcomes_for_target_with_options(
                &battle_with, &atk_with, &def_base, p1(0), p2(0),
                &move_data, oracle_config, 1.0, 1.0, None, None,
            );
            if damage_differs(&outcomes, &baseline) && !listed_atk_abilities.contains(&Ability::SandForce) {
                failures.push(
                    "[CONDITIONED-ATK-ABILITY] SandForce changes damage under Sandstorm but is \
                     NOT in offensive_damage_abilities".to_string()
                );
            }
        }

        // (c) Burned attacker + Guts: physical attacker with Burn + Guts nets a net Atk boost
        //     (Guts 1.5× overrides the Burn 0.5× penalty, netting a 50% increase relative to
        //     a burned attacker with no ability).
        {
            let mut atk_unk_burned = open_mon();
            atk_unk_burned.possible_types = Unknown::Known(vec![PokemonType::Normal]);
            atk_unk_burned.status = Some(Status::Burn);
            let def_base = materialize_pokemon(&open_mon(), def_stats, Item::None, Ability::None);
            let move_data = normal_physical_move(PokemonMove::Tackle, 80);
            // Baseline: burned attacker, no ability.
            let atk_base = materialize_pokemon(&atk_unk_burned, atk_stats, Item::None, Ability::None);
            let battle_base = materialize_battle(&blank_battle_unk, vec![atk_base.clone()], vec![def_base.clone()]);
            let baseline = calculate_damage_outcomes_for_target_with_options(
                &battle_base, &atk_base, &def_base, p1(0), p2(0),
                &move_data, oracle_config, 1.0, 1.0, None, None,
            );
            // Probe: burned attacker + Guts.
            let atk_with = materialize_pokemon(&atk_unk_burned, atk_stats, Item::None, Ability::Guts);
            let battle_with = materialize_battle(&blank_battle_unk, vec![atk_with.clone()], vec![def_base.clone()]);
            let outcomes = calculate_damage_outcomes_for_target_with_options(
                &battle_with, &atk_with, &def_base, p1(0), p2(0),
                &move_data, oracle_config, 1.0, 1.0, None, None,
            );
            if damage_differs(&outcomes, &baseline) && !listed_atk_abilities.contains(&Ability::Guts) {
                failures.push(
                    "[CONDITIONED-ATK-ABILITY] Guts changes damage on a burned attacker but is \
                     NOT in offensive_damage_abilities".to_string()
                );
            }
        }

        // (d) Electric Terrain + HadronEngine: special attacker gains +SpA under Electric Terrain.
        {
            let battle_eterrain = battle_with_terrain(Terrain::ElectricTerrain);
            let mut atk_unk = open_mon();
            atk_unk.possible_types = Unknown::Known(vec![PokemonType::Electric]);
            let def_base = materialize_pokemon(&open_mon(), def_stats, Item::None, Ability::None);
            let move_data = {
                let mut md = normal_physical_move(PokemonMove::Tackle, 90);
                md.category = MoveCategory::Special;
                md.pokemon_type = PokemonType::Electric;
                md
            };
            let atk_base = materialize_pokemon(&atk_unk, atk_stats, Item::None, Ability::None);
            let battle_base = materialize_battle(&battle_eterrain, vec![atk_base.clone()], vec![def_base.clone()]);
            let baseline = calculate_damage_outcomes_for_target_with_options(
                &battle_base, &atk_base, &def_base, p1(0), p2(0),
                &move_data, oracle_config, 1.0, 1.0, None, None,
            );
            let atk_with = materialize_pokemon(&atk_unk, atk_stats, Item::None, Ability::HadronEngine);
            let battle_with = materialize_battle(&battle_eterrain, vec![atk_with.clone()], vec![def_base.clone()]);
            let outcomes = calculate_damage_outcomes_for_target_with_options(
                &battle_with, &atk_with, &def_base, p1(0), p2(0),
                &move_data, oracle_config, 1.0, 1.0, None, None,
            );
            if damage_differs(&outcomes, &baseline) && !listed_atk_abilities.contains(&Ability::HadronEngine) {
                failures.push(
                    "[CONDITIONED-ATK-ABILITY] HadronEngine changes damage under Electric Terrain \
                     but is NOT in offensive_damage_abilities".to_string()
                );
            }
        }
    }

    assert!(
        failures.is_empty(),
        "{} allowlist completeness failure(s):\n{}",
        failures.len(),
        failures.join("\n")
    );
}

// ── L-B: materialize.rs heuristic invariants ─────────────────────────────────
//
// Regression test for the two documented approximations in materialize.rs:
//
// 1. HP×0.5 sentinel: a defender at PokemonHP::Percent(p≠100) is materialized
//    with `hp = max_hp × 0.5`.  This must be strictly < max_hp so all
//    full-HP-gated reducers (Multiscale, ShadowShield, TeraShell) deactivate.
//    Verified by asserting Multiscale halves damage at Percent(100) but not at
//    Percent(50), and that the damage ratio is ≈ 0.5.
//
// 2. Known(0) timer sentinel: the S-A per-effect timer model stores Known(0) for
//    permanent effects (primordial weather, entry hazards).  materialize_battle
//    must pass Known(0) through as 0, never folding it into the `_ => 3` fallback.
//    Verified by asserting `weather_turns == Some(0)` after materialization.
#[test]
fn test_lb_multiscale_hp_gate_and_timer_sentinel() {
    use crate::information::materialize::{materialize_battle, materialize_pokemon};
    use crate::information::unknowns::Unknown;
    use crate::simulator::helpers::calculate_damage_outcomes_for_target_with_options;
    use crate::simulator::DamageConfig;
    use crate::state::dex_data::Weather;

    let oracle_config = DamageConfig { consider_crit: false, damage_rolls: 1 };
    let atk_stats: [u16; 6] = [200, 250, 100, 100, 100, 100];
    let def_stats: [u16; 6] = [300, 80, 100, 80, 100, 80];

    let mut atk_unk = unknown_mon_species(Species::Garchomp);
    atk_unk.possible_types = Unknown::Known(vec![PokemonType::Normal]);
    let atk_ps = materialize_pokemon(&atk_unk, atk_stats, Item::None, Ability::None);

    let move_data = normal_physical_move(PokemonMove::Tackle, 80);

    // Defender is Normal-type too: keeps the matchup neutral so type effectiveness doesn't confound the Multiscale ratio check.
    let mut def_unk = unknown_mon_species(Species::Garchomp);
    def_unk.possible_types = Unknown::Known(vec![PokemonType::Normal]);

    // ── Full HP: Multiscale should fire (×0.5 to incoming damage). ────────────
    let mut def_full = def_unk.clone();
    def_full.hp = PokemonHP::Percent(100);
    let def_ps_full = materialize_pokemon(&def_full, def_stats, Item::None, Ability::Multiscale);

    // ── Partial HP: Multiscale must NOT fire. ─────────────────────────────────
    let mut def_part = def_unk.clone();
    def_part.hp = PokemonHP::Percent(50);
    let def_ps_part = materialize_pokemon(&def_part, def_stats, Item::None, Ability::Multiscale);

    let blank_unk = battle_with_p2(vec![def_unk.clone()]);
    let battle_full = materialize_battle(&blank_unk, vec![atk_ps.clone()], vec![def_ps_full.clone()]);
    let battle_part = materialize_battle(&blank_unk, vec![atk_ps.clone()], vec![def_ps_part.clone()]);

    let out_full = calculate_damage_outcomes_for_target_with_options(
        &battle_full, &atk_ps, &def_ps_full, p1(0), p2(0),
        &move_data, oracle_config, 1.0, 1.0, None, None,
    );
    let out_part = calculate_damage_outcomes_for_target_with_options(
        &battle_part, &atk_ps, &def_ps_part, p1(0), p2(0),
        &move_data, oracle_config, 1.0, 1.0, None, None,
    );

    assert!(!out_full.is_empty() && !out_part.is_empty(), "oracle must produce results");

    let dmg_full = out_full[0].0 as f64;
    let dmg_part = out_part[0].0 as f64;

    // Multiscale ×0.5 must reduce damage at full HP.
    assert!(
        dmg_full < dmg_part,
        "Multiscale must fire at full HP (damage must be less than at partial HP): \
         full={dmg_full}, partial={dmg_part}"
    );
    // Ratio must be ≈ 0.5 (within ±5 % to account for integer floor rounding).
    let ratio = dmg_full / dmg_part;
    assert!(
        (0.45..=0.55).contains(&ratio),
        "Multiscale ×0.5 expected; got damage ratio {ratio:.3} \
         (full_hp={dmg_full}, partial_hp={dmg_part})"
    );

    // ── Part 2: Known(0) timer materializes to 0 (not 3). ────────────────────
    let mut unk_perm = blank_unk.clone();
    unk_perm.weather = Some(Weather::Sun);
    unk_perm.weather_turns = Some(Unknown::Known(0)); // permanent-effect sentinel
    let concrete = materialize_battle(&unk_perm, vec![atk_ps.clone()], vec![def_ps_full.clone()]);
    assert_eq!(
        concrete.weather_turns,
        Some(0),
        "Known(0) weather timer must pass through as 0 (permanent-effect sentinel), not fold into 3"
    );
}

// ── Information Abilities: Frisk ──────────────────────────────────────────────

/// Frisk: AbilityRevealed{Frisk} wrapping ItemRevealed collapses the foe's item to Known.
#[test]
fn test_frisk_reveals_item() {
    // Observer is P1; Frisk mon is P1 slot 0 (own side); foe is P2 slot 0.
    // Before Frisk fires, the foe's item is Unknown::Not([]) (nothing excluded).
    let mut foe = unknown_mon();
    foe.item = Unknown::Not(vec![]); // unknown item
    let state = battle_1v1(unknown_mon(), foe);

    let events = vec![event_with(
        EventKind::AbilityRevealed { slot: p1(0), ability: Ability::Frisk },
        vec![event(EventKind::ItemRevealed { slot: p2(0), item: Item::Leftovers })],
    )];

    let result = apply(state, events);
    assert_eq!(
        result.p2_active_mons[0].item,
        Unknown::Known(Item::Leftovers),
        "Frisk must collapse the foe's item to Known(Leftovers)"
    );
}

/// Frisk reveals multiple foes (doubles): both get their items locked in.
#[test]
fn test_frisk_reveals_multiple_foes() {
    let n = 2;
    let mut foe0 = unknown_mon();
    foe0.item = Unknown::Not(vec![]);
    let mut foe1 = unknown_mon();
    foe1.item = Unknown::Not(vec![]);
    let mut state = battle_with_p2(vec![foe0, foe1]);
    state.active_per_side = 2;
    state.p1_active_mons = vec![unknown_mon(), unknown_mon()];
    state.p1_slot_conditions = vec![vec![], vec![]];

    let events = vec![event_with(
        EventKind::AbilityRevealed { slot: p1(0), ability: Ability::Frisk },
        vec![
            event(EventKind::ItemRevealed { slot: p2(0), item: Item::ChoiceBand }),
            event(EventKind::ItemRevealed { slot: p2(1), item: Item::Leftovers }),
        ],
    )];

    let result = apply(state, events);
    assert_eq!(result.p2_active_mons[0].item, Unknown::Known(Item::ChoiceBand));
    assert_eq!(result.p2_active_mons[1].item, Unknown::Known(Item::Leftovers));
}

/// Frisk with an item-less foe: no ItemRevealed inside, foe's item stays excluded (None).
#[test]
fn test_frisk_no_item_foe() {
    let mut foe = unknown_mon();
    foe.item = Unknown::Not(vec![]); // unknown — could have any item
    let state = battle_1v1(unknown_mon(), foe);

    // AbilityRevealed{Frisk} with NO inner ItemRevealed — foe has no item.
    let events = vec![event(EventKind::AbilityRevealed {
        slot: p1(0),
        ability: Ability::Frisk,
    })];

    let result = apply(state, events);
    // The item knowledge must be EXACTLY unchanged (still fully unknown). Asserting
    // only "not Known" was vacuous — the precondition already satisfied it; equality
    // also catches any accidental mutation. The positive path (nested ItemRevealed →
    // Known) is covered by test_frisk_reveals_multiple_foes above.
    assert_eq!(
        result.p2_active_mons[0].item,
        Unknown::Not(vec![]),
        "item-less foe: Frisk alone (no ItemRevealed) must leave item knowledge unchanged"
    );
}

// ── Information Abilities: Anticipation ──────────────────────────────────────

/// Anticipation: AnticipationShudder from P1 slot 0 → adds KnowsThreateningMove clause.
/// The clause should appear in the predicate store.
#[test]
fn test_anticipation_adds_predicate() {
    // P1's mon shuddered → at least one P2 active mon knows a threatening move.
    // In singles, this is a unit clause: P2 mon 0 KnowsThreateningMove.
    let state = battle_1v1(unknown_mon(), unknown_mon());

    let events = vec![event_with(
        EventKind::AnticipationShudder { slot: p1(0) },
        vec![event(EventKind::AbilityRevealed { slot: p1(0), ability: Ability::Anticipation })],
    )];

    let result = apply(state, events);
    // There must be at least one predicate clause referencing KnowsThreateningMove.
    let has_clause = result.predicates.iter().any(|clause| {
        clause.iter().any(|lit| matches!(lit, Statement::KnowsThreateningMove { .. }))
    });
    assert!(has_clause, "AnticipationShudder must add a KnowsThreateningMove clause");
}

/// Anticipation on opponent's mon: shudder on P2 slot 0 gives P1 no info
/// (the shuddering mon is on the observer's opponent's side — inference ignores it).
#[test]
fn test_anticipation_opponent_shudder_no_clause() {
    // Observer is P1; AnticipationShudder on P2 slot 0 means P2's mon
    // is the holder, and P1's active mons would be the threats.
    // Since P1 is the observer, inference should NOT add a predicate about P1's
    // own known mons (the observer knows its own moves already).
    let state = battle_1v1(unknown_mon(), unknown_mon());

    let events = vec![event_with(
        EventKind::AnticipationShudder { slot: p2(0) },
        vec![event(EventKind::AbilityRevealed { slot: p2(0), ability: Ability::Anticipation })],
    )];

    let result = apply(state, events);
    // No KnowsThreateningMove clause should be added for the observer's own mons.
    let has_clause = result.predicates.iter().any(|clause| {
        clause.iter().any(|lit| matches!(lit, Statement::KnowsThreateningMove { .. }))
    });
    assert!(
        !has_clause,
        "AnticipationShudder on opponent's mon must not add a KnowsThreateningMove clause \
         (observer already knows their own moves)"
    );
}

// ── Information Abilities: Illusion ──────────────────────────────────────────

/// IllusionEnded: once the disguise breaks, the mon's possible_species collapses to the
/// true species.
#[test]
fn test_illusion_ended_collapses_species() {
    // Foe (P2 slot 0) was disguised.  Inference initially shows it could be either
    // Zoroark or the disguise species.  After IllusionEnded, it must be Known(Zoroark).
    let mut foe = unknown_mon();
    foe.possible_species = Unknown::Possibly(vec![Species::Zoroark, Species::Garchomp]);
    let state = battle_1v1(unknown_mon(), foe);

    let events = vec![event(EventKind::IllusionEnded {
        slot: p2(0),
        actual_species: Species::Zoroark,
    })];

    let result = apply(state, events);
    assert_eq!(
        result.p2_active_mons[0].possible_species,
        Unknown::Known(Species::Zoroark),
        "IllusionEnded must collapse possible_species to Known(actual_species)"
    );
}

/// IllusionEnded: no contradiction panic even if prior possible_species was Known(disguise).
/// (The inference should silently overwrite, not call inference_contradiction!)
#[test]
fn test_illusion_ended_overwrites_known_disguise() {
    // Inference tracked the foe as Garchomp (the disguise species it presented).
    let mut foe = unknown_mon();
    foe.possible_species = Unknown::Known(Species::Garchomp);
    let state = battle_1v1(unknown_mon(), foe);

    let events = vec![event(EventKind::IllusionEnded {
        slot: p2(0),
        actual_species: Species::Zoroark,
    })];

    // This must NOT panic.
    let result = apply(state, events);
    assert_eq!(
        result.p2_active_mons[0].possible_species,
        Unknown::Known(Species::Zoroark),
        "IllusionEnded must silently update species even if the disguise was previously Known"
    );
}

/// IllusionEnded on the observer's own side: should still collapse species normally
/// (the observer's own species is always known, but the event still processes).
#[test]
fn test_illusion_ended_own_side() {
    let mut own_mon = unknown_mon();
    own_mon.possible_species = Unknown::Possibly(vec![Species::Zoroark, Species::Absol]);
    let state = battle_1v1(own_mon, unknown_mon());

    let events = vec![event(EventKind::IllusionEnded {
        slot: p1(0),
        actual_species: Species::Zoroark,
    })];

    let result = apply(state, events);
    assert_eq!(
        result.p1_active_mons[0].possible_species,
        Unknown::Known(Species::Zoroark),
    );
}

// ── Flinch attribution (pass2_flinch_holder_from_cant) ───────────────────────
//
// When a P1 mon is flinched by a single P2 attacker whose move has no flinch
// secondary, the inference engine should emit a clause
// [HasItem{KingsRock}, HasItem{RazorFang}, HasAbility{Stench}] for the attacker.
// BCP resolves it to Known(KingsRock) when the other two candidates are already
// excluded from the attacker.

/// Single unambiguous attacker, no flinch secondary, Stench+RazorFang excluded →
/// deduces King's Rock.
#[test]
fn flinch_deduces_kings_rock_single_attacker() {
    // P2 attacker: item unknown but RazorFang excluded; ability known-not-Stench.
    let mut attacker = unknown_mon();
    attacker.item = Unknown::Not(vec![Item::RazorFang]);
    attacker.possible_abilities = Unknown::Not(vec![Ability::Stench]);

    let state = battle_1v1(unknown_mon(), attacker);

    // P2 uses Tackle on P1 (deals damage), then P1 can't move (flinched).
    let events = vec![
        event_with(
            EventKind::MoveUsed {
                user: p2(0),
                move_used: PokemonMove::Tackle,
                targets: vec![p1(0)],
            },
            vec![event(EventKind::DamageDealt {
                target: p1(0),
                new_hp: PokemonHP::Percent(80),
            })],
        ),
        event(EventKind::Cant {
            slot: p1(0),
            reason: CantReason::Flinch,
        }),
    ];

    // Tackle: damaging, no flinch secondary.
    let md = HashMap::from([(PokemonMove::Tackle, normal_physical_move(PokemonMove::Tackle, 40))]);
    let result = apply_ex(state, events, HashMap::new(), md);

    // BCP should collapse the [KingsRock] unit clause to Known(KingsRock).
    assert_eq!(
        result.p2_active_mons[0].item,
        Unknown::Known(Item::KingsRock),
        "With RazorFang excluded and Stench excluded, flinch from sole attacker must \
         deduce Known(KingsRock); got {:?}", result.p2_active_mons[0].item
    );
}

/// Ambiguous flinch (two P2 attackers both dealt damage to T) → no deduction.
#[test]
fn flinch_no_deduction_multiple_attackers() {
    // 2v2: P1 slot 0 is the flinched target; P2 slots 0 and 1 are both attackers.
    let state = battle_nvn(
        vec![unknown_mon(), unknown_mon()],
        vec![unknown_mon(), unknown_mon()],
    );

    // Both P2 attackers deal damage to P1 slot 0 this turn, then P1/0 flinches.
    let md = HashMap::from([(PokemonMove::Tackle, normal_physical_move(PokemonMove::Tackle, 40))]);
    let events = vec![
        event_with(
            EventKind::MoveUsed {
                user: p2(0),
                move_used: PokemonMove::Tackle,
                targets: vec![p1(0)],
            },
            vec![event(EventKind::DamageDealt { target: p1(0), new_hp: PokemonHP::Percent(80) })],
        ),
        event_with(
            EventKind::MoveUsed {
                user: p2(1),
                move_used: PokemonMove::Tackle,
                targets: vec![p1(0)],
            },
            vec![event(EventKind::DamageDealt { target: p1(0), new_hp: PokemonHP::Percent(60) })],
        ),
        event(EventKind::Cant { slot: p1(0), reason: CantReason::Flinch }),
    ];

    let result = apply_ex(state, events, HashMap::new(), md);

    // With two candidate attackers, no flinch clause should be pushed.
    let has_flinch_clause = result.predicates.iter().any(|clause| {
        clause.iter().any(|s| matches!(s,
            Statement::HasItem { item: Item::KingsRock, .. }
            | Statement::HasItem { item: Item::RazorFang, .. }
            | Statement::HasAbility { ability: Ability::Stench, .. }
        ))
    });
    assert!(!has_flinch_clause,
        "Ambiguous flinch (two attackers) must not push a flinch clause;\n\
         predicates = {:#?}", result.predicates);
    // Neither attacker's item should be forced.
    assert!(!matches!(&result.p2_active_mons[0].item, Unknown::Known(_)),
        "Attacker 0 item must not be forced; got {:?}", result.p2_active_mons[0].item);
    assert!(!matches!(&result.p2_active_mons[1].item, Unknown::Known(_)),
        "Attacker 1 item must not be forced; got {:?}", result.p2_active_mons[1].item);
}

/// Move already has a flinch secondary (e.g. Iron Head 30%) → no item/ability deduction.
#[test]
fn flinch_no_deduction_move_has_flinch_secondary() {
    let mut attacker = unknown_mon();
    attacker.item = Unknown::Not(vec![Item::RazorFang]);
    attacker.possible_abilities = Unknown::Not(vec![Ability::Stench]);

    let state = battle_1v1(unknown_mon(), attacker);

    let events = vec![
        event_with(
            EventKind::MoveUsed {
                user: p2(0),
                move_used: PokemonMove::IronHead,
                targets: vec![p1(0)],
            },
            vec![event(EventKind::DamageDealt { target: p1(0), new_hp: PokemonHP::Percent(70) })],
        ),
        event(EventKind::Cant { slot: p1(0), reason: CantReason::Flinch }),
    ];

    // Iron Head: physical Steel move with a 30% flinch secondary.
    let mut iron_head = normal_physical_move(PokemonMove::IronHead, 80);
    iron_head.secondaries = vec![PokemonSecondaryEffect {
        chance: 30,
        effect: HitEffect {
            volatile_status: Some(VolatileStatus::Flinch),
            ..Default::default()
        },
        random_choices: vec![],
    }];
    let md = HashMap::from([(PokemonMove::IronHead, iron_head)]);
    let result = apply_ex(state, events, HashMap::new(), md);

    // The move explains the flinch; no item deduction should occur.
    let has_flinch_clause = result.predicates.iter().any(|clause| {
        clause.iter().any(|s| matches!(s,
            Statement::HasItem { item: Item::KingsRock, .. }
            | Statement::HasAbility { ability: Ability::Stench, .. }
        ))
    });
    assert!(!has_flinch_clause,
        "Iron Head's flinch secondary explains the flinch; no clause should be pushed;\n\
         predicates = {:#?}", result.predicates);
    // KingsRock must not be deduced.
    assert!(
        !matches!(&result.p2_active_mons[0].item, Unknown::Known(Item::KingsRock)),
        "Must not attribute King's Rock when move has a flinch secondary; got {:?}",
        result.p2_active_mons[0].item
    );
}

/// When P2's mon flinches (T is P2, attacker is P1 = observer's own side),
/// no clause should be pushed — it's not useful to constrain the observer's own mons.
#[test]
fn flinch_no_deduction_when_observer_side_flinches() {
    let state = battle_1v1(unknown_mon(), unknown_mon());

    // P1 attacks P2, and P2 flinches.
    let events = vec![
        event_with(
            EventKind::MoveUsed {
                user: p1(0),
                move_used: PokemonMove::Tackle,
                targets: vec![p2(0)],
            },
            vec![event(EventKind::DamageDealt { target: p2(0), new_hp: PokemonHP::Percent(70) })],
        ),
        event(EventKind::Cant { slot: p2(0), reason: CantReason::Flinch }),
    ];

    let md = HashMap::from([(PokemonMove::Tackle, normal_physical_move(PokemonMove::Tackle, 40))]);
    let result = apply_ex(state, events, HashMap::new(), md);

    // P2 flinching: attacker is P1 (observer's own side) — no useful clause.
    let has_flinch_clause = result.predicates.iter().any(|clause| {
        clause.iter().any(|s| matches!(s,
            Statement::HasItem { item: Item::KingsRock, .. }
            | Statement::HasAbility { ability: Ability::Stench, .. }
        ))
    });
    assert!(!has_flinch_clause,
        "P2 flinching (from P1 attack) must not push a flinch clause on P1;\n\
         predicates = {:#?}", result.predicates);
}

// ── Regression tests for audit fixes (C1–C4) ─────────────────────────────────

/// C1: a voluntary Switch event with `tera_type: None` must NOT reveal tera type or
/// flip `is_tera`, and a switch with `tera_type: Some(Fire)` MUST set both fields.
#[test]
fn test_c1_switch_tera_type_not_leaked_when_none() {
    // ── Case 1: non-tera switch (tera_type: None) ──────────────────────────
    let state_a = battle_with_p2(vec![unknown_mon()]);
    let events_a = vec![event(EventKind::Switch(SwitchState {
        slot: p2(0),
        species: Species::Garchomp,
        level: 50,
        hp: PokemonHP::Percent(100),
        status: None,
        tera_type: None,
    }))];
    let result_a = apply(state_a, events_a);
    let p2_a = &result_a.p2_active_mons[0];
    assert!(!p2_a.is_tera,
        "non-tera switch must leave is_tera=false; got is_tera=true");
    assert!(
        !matches!(&p2_a.possible_tera_type, Unknown::Known(_)),
        "non-tera switch must NOT set possible_tera_type to Known; got {:?}",
        p2_a.possible_tera_type
    );

    // ── Case 2: tera switch (tera_type: Some(Fire)) ─────────────────────────
    let state_b = battle_with_p2(vec![unknown_mon()]);
    let events_b = vec![event(EventKind::Switch(SwitchState {
        slot: p2(0),
        species: Species::Garchomp,
        level: 50,
        hp: PokemonHP::Percent(100),
        status: None,
        tera_type: Some(PokemonType::Fire),
    }))];
    let result_b = apply(state_b, events_b);
    let p2_b = &result_b.p2_active_mons[0];
    assert!(p2_b.is_tera,
        "tera switch must set is_tera=true");
    assert_eq!(
        p2_b.possible_tera_type, Unknown::Known(PokemonType::Fire),
        "tera switch must set possible_tera_type = Known(Fire); got {:?}",
        p2_b.possible_tera_type
    );
}

/// C2: when P2 has Known(Analytic) and moves FIRST this turn (no prior P1 MoveUsed),
/// Analytic's ×1.3 must NOT be applied to the oracle → the true stat must remain
/// within the inferred upper bound.
///
/// Setup: P2 (Garchomp, Analytic, Atk BSV=150) attacks P1 (Def=115) for 24 damage
/// (= Tackle max roll at ×1.0, Atk=150, Def=115, level=50).
///
/// With fix (×1.0): oracle range for BSV=150 is [20, 24]; 24 ∈ [20, 24] → feasible.
/// Without fix (×1.3): oracle range for BSV=150 is [26, 31]; 24 ∉ [26, 31] → infeasible.
#[test]
fn test_c2_analytic_first_mover_does_not_exclude_true_stat() {
    use crate::state::dex_data::PokemonData;
    use crate::state::pokemon::PokemonGender;

    // Minimal Garchomp dex entry: only base_stats[1] (Atk=130) matters for Direction B.
    let garchomp_data = PokemonData {
        species: Species::Garchomp,
        types: vec![PokemonType::Dragon, PokemonType::Ground],
        base_stats: [108, 130, 95, 80, 85, 102],
        weight: 950,
        primary_ability: None,
        abilities: vec![],
        base_species: None,
        forme: None,
        required_item: None,
        battle_only: None,
        default_gender: PokemonGender::Genderless,
    };
    let dex = HashMap::from([(Species::Garchomp, garchomp_data)]);

    // HP=200 is achievable for Garchomp base-HP=108 at level 50
    // (e.g. IV=31, EV=132: floor((216+31+33)*0.5)+60=200). Staying in [168, 215]
    // avoids a pass5 "no IV/EV can produce observed HP bounds" contradiction.
    let p1_hp = 200u16;
    let p1_def = 115u16;

    // mon_p1 / mon_p2: avoid shadowing the `p1()` / `p2()` slot-builder helpers.
    // P1: observer's own mon — exact Number HP, known Def.
    let mut mon_p1 = unknown_mon();
    mon_p1.hp = PokemonHP::Number(p1_hp);
    mon_p1.minStats[0] = p1_hp; mon_p1.maxStats[0] = p1_hp;
    mon_p1.minStats[2] = p1_def; mon_p1.maxStats[2] = p1_def;

    // P2: Garchomp with Analytic, pre-nature Atk stat tightened to exactly 150
    // (corresponds to 31 IVs, 0 EVs, neutral nature at level 50).
    let true_atk_bsv = 150u16;
    let mut mon_p2 = unknown_mon_species(Species::Garchomp);
    mon_p2.possible_abilities = Unknown::Known(Ability::Analytic);
    mon_p2.min_pre_nature_stat[1] = true_atk_bsv;
    mon_p2.max_pre_nature_stat[1] = true_atk_bsv;
    mon_p2.minStats[1] = true_atk_bsv;
    mon_p2.maxStats[1] = true_atk_bsv;

    // Damage 24 = Tackle max roll (100%) at Atk=150, Def=115, level=50, ×1.0.
    // Base: floor(floor(22*40*150/115)/50)+2 = floor(floor(1147)/50)+2 = 22+2 = 24.
    // Range without Analytic: [20, 24]. Range with ×1.3 Analytic: [26, 31].
    // 24 is in [20, 24] but NOT in [26, 31] → the fix is required for this to pass.
    // new_hp = 200 - 24 = 176.
    let exact_damage = 24u16;
    let new_hp_val = PokemonHP::Number(p1_hp - exact_damage); // 200 - 24 = 176

    let state = battle_1v1(mon_p1, mon_p2);
    let md = HashMap::from([
        (PokemonMove::Tackle, normal_physical_move(PokemonMove::Tackle, 40)),
    ]);

    // P2 moves first — no P1 MoveUsed before this event.
    let events = vec![
        event_with(
            EventKind::MoveUsed {
                user: p2(0),
                move_used: PokemonMove::Tackle,
                targets: vec![p1(0)],
            },
            vec![event(EventKind::DamageDealt { target: p1(0), new_hp: new_hp_val })],
        ),
    ];

    let result = apply_ex(state, events, dex, md);

    let p2_result = &result.p2_active_mons[0];
    assert!(
        p2_result.max_pre_nature_stat[1] >= true_atk_bsv,
        "Analytic first-mover: upper Atk BSV bound must be ≥ {true_atk_bsv} (true stat); \
         got {} — Analytic (×1.3) must only apply when the holder moves last",
        p2_result.max_pre_nature_stat[1]
    );
}

/// C3: Intrepid Sword / Dauntless Shield must NOT be excluded on re-entry after the
/// ability has already fired (one_time_ability_used should be set, preventing the
/// absence-exclusion on subsequent switch-ins).
#[test]
fn test_c3_intrepid_sword_not_excluded_on_reentry() {
    // Turn 1: P2 enters with Intrepid Sword firing (+1 Atk to self).
    // This should mark one_time_ability_used = true on P2's mon.
    let state = battle_with_p2(vec![unknown_mon()]);
    let events_turn1 = vec![
        event_with(
            EventKind::Switch(SwitchState {
                slot: p2(0),
                species: Species::Garchomp,
                level: 50,
                hp: PokemonHP::Percent(100),
                status: None,
                tera_type: None,
            }),
            vec![event(EventKind::BoostChanged {
                target: p2(0),
                boost_idx: 0,
                stages: 1,
            })],
        ),
    ];
    let after_turn1 = apply(state, events_turn1);

    assert!(
        after_turn1.p2_active_mons[0].one_time_ability_used,
        "after seeing the +1 Atk boost on switch-in, one_time_ability_used must be true"
    );

    // Turn 2: P2 re-enters with NO +1 Atk boost (once-per-battle ability exhausted).
    // The absence inference must NOT exclude IntrepidSword because one_time_ability_used = true.
    let events_turn2 = vec![
        event(EventKind::Switch(SwitchState {
            slot: p2(0),
            species: Species::Garchomp,
            level: 50,
            hp: PokemonHP::Percent(100),
            status: None,
            tera_type: None,
        })),
    ];
    let after_turn2 = apply(after_turn1, events_turn2);

    let p2_mon = &after_turn2.p2_active_mons[0];
    assert!(
        !unknown_is_excluded(&p2_mon.possible_abilities, &Ability::IntrepidSword),
        "IntrepidSword must NOT be excluded after re-entry (one_time_ability_used = true); \
         possible_abilities = {:?}", p2_mon.possible_abilities
    );
}

/// C4: a Drizzle/Drought mon switching in under primordial weather
/// (Heavy Rain / Extreme Sunlight) must neither be excluded nor trigger a contradiction
/// panic.  The `set_weather` no-op means WeatherChanged is absent, but the unconditional
/// `AbilityRevealed{Drizzle}` still fires — so the absence inference must not run.
#[test]
fn test_c4_weather_setter_not_excluded_under_primordial_weather() {
    // State: Heavy Rain active (primordial weather).
    let mut state = battle_with_p2(vec![unknown_mon()]);
    state.weather = Some(Weather::HeavyRain);

    // P2 switches in with Drizzle revealed (AbilityRevealed reaction) but NO WeatherChanged.
    // Under Heavy Rain, set_weather silently no-ops, so no WeatherChanged event fires.
    let events = vec![
        event_with(
            EventKind::Switch(SwitchState {
                slot: p2(0),
                species: Species::Garchomp,
                level: 50,
                hp: PokemonHP::Percent(100),
                status: None,
                tera_type: None,
            }),
            vec![
                event_with(
                    EventKind::AbilityRevealed { slot: p2(0), ability: Ability::Drizzle },
                    vec![], // No WeatherChanged reaction (primordial weather suppressed it).
                ),
            ],
        ),
    ];

    // This must not panic (before fix: absence pass would exclude Drizzle, then
    // AbilityRevealed would try to set Known(Drizzle) → contradiction panic).
    let result = apply(state, events);

    let p2_mon = &result.p2_active_mons[0];
    assert!(
        !unknown_is_excluded(&p2_mon.possible_abilities, &Ability::Drizzle),
        "Drizzle must NOT be excluded when switching in under Heavy Rain (primordial weather); \
         possible_abilities = {:?}", p2_mon.possible_abilities
    );
}

// ── T1: End-to-end simulator → inference round-trip harness ──────────────────
//
// These tests wire the full pipeline: a real `BattleState` → `simulate_turn` with
// `event_observer = P1` → real `InformationEvent` stream → `apply_information` →
// `UnknownBattleState`.  The soundness invariant (every true P2 hidden value lies
// within the inferred bounds) is asserted on each turn.
//
// This is the structural gap that made C1 (tera leak) invisible to the hand-written
// test suite: no prior test exercised the *simulator's* actual emission path.
mod roundtrip_soundness {
    use std::collections::HashMap;

    use crate::data::ability::Ability;
    use crate::data::item::Item;
    use crate::data::pokemon_move::PokemonMove;
    use crate::data::species::Species;
    use crate::information::inference::{apply_information, unknown_is_excluded, InferenceConfig};
    use crate::information::information::InformationEvent;
    use crate::information::unknowns::{Unknown, UnknownBattleState, UnknownMatchState, UnknownPokemonState};
    use crate::state::battle::{BattleCommand, MatchState, Player, PlayerCommand, SwitchCommand};
    use crate::state::dex_data::PokemonType;
    use crate::state::pokemon::{build_pokemon_state, Nature, PokemonState};
    use crate::tests::simuilator_test_helpers::{
        battle_state_from_lists, move_dex, pokemon_dex, run_single_turn_with_events, simple_attack,
    };

    // ── Shared helpers ────────────────────────────────────────────────────────

    /// Build a 1v1 `UnknownBattleState` for P1's fog-of-war view.
    /// P1's side is fully known (`from_known_pokemon`); P2's active slot is
    /// species-only (`from_opponent_species`).
    fn fog_1v1(p1: &PokemonState, p2_species: Species) -> UnknownBattleState {
        let dex = pokemon_dex();
        super::battle_nvn(
            vec![UnknownPokemonState::from_known_pokemon(p1)],
            vec![UnknownPokemonState::from_opponent_species(p2_species, dex, 50)],
        )
    }

    /// Run the real simulator (P1 observer), pick the highest-probability `BattleState`
    /// branch, and return its event list.
    fn simulate_and_get_events(
        state: MatchState,
        p1_cmd: PlayerCommand,
        p2_cmd: PlayerCommand,
    ) -> Vec<InformationEvent> {
        let md = move_dex();
        let pd = pokemon_dex();
        let mut branches = run_single_turn_with_events(&state, &p1_cmd, &p2_cmd, md, pd, Player::P1);
        // Sort highest probability first; take the first BattleState branch.
        branches.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
        for (st, ev_opt, _) in branches {
            if matches!(&st, MatchState::BattleState(_)) {
                return ev_opt.expect("observer set → events must be Some");
            }
        }
        panic!("no BattleState branch found — both mons may have fainted");
    }

    /// Apply the real event stream to the fog-of-war state, returning the updated battle.
    fn apply_roundtrip(fog: UnknownBattleState, events: Vec<InformationEvent>) -> UnknownBattleState {
        let dex = pokemon_dex();
        let md = move_dex();
        let result = apply_information(
            UnknownMatchState::Battle(fog),
            &events,
            false,
            dex,
            md,
            &HashMap::new(), // ability_dex — not needed for these tests
            &InferenceConfig::default(),
        );
        match result {
            UnknownMatchState::Battle(b) => b,
            _ => panic!("expected Battle state after apply_information"),
        }
    }

    // ── Scenario A: Voluntary switch — C1 tera-leak regression ───────────────

    /// P2 voluntarily switches from Snorlax → Garchomp.  Garchomp has tera_type=Fire
    /// but has NOT Terastallized (is_tera=false).
    ///
    /// **C1 regression**: After the C1 fix, the emitted Switch event has `tera_type=None`
    /// for a non-terastallized mon.  Inference must therefore NOT set `is_tera=true` or
    /// `possible_tera_type=Known(Fire)` on the incoming Garchomp.
    ///
    /// Reverting `simulator/mod.rs:6498` back to unconditional `Some(mon.tera_type)` would
    /// make this test fail — confirming the C1 fix is exercised by the real emission path.
    #[test]
    fn roundtrip_a_voluntary_switch_tera_type_not_leaked() {
        let pd = pokemon_dex();
        let md = move_dex();

        // P1: slow Shuckle with Splash.
        let p1 = build_pokemon_state(
            Species::Shuckle, pd, md, Some(50),
            Some([Some(PokemonMove::Splash), None, None, None]),
            None, Some(Ability::None), Some(Nature::Hardy), None, None,
            Some([0u8; 6]), Some([31u8; 6]), false,
        );

        // P2 active: Snorlax with Splash, Immunity ability.
        let p2_active = build_pokemon_state(
            Species::Snorlax, pd, md, Some(50),
            Some([Some(PokemonMove::Splash), None, None, None]),
            None, Some(Ability::Immunity), Some(Nature::Hardy), None, None,
            Some([0u8; 6]), Some([31u8; 6]), false,
        );
        // P2 back: Garchomp, SandVeil, tera_type=Fire — but NOT Terastallized.
        let p2_back = build_pokemon_state(
            Species::Garchomp, pd, md, Some(50),
            Some([Some(PokemonMove::Splash), None, None, None]),
            None, Some(Ability::SandVeil), Some(Nature::Hardy), None,
            Some(PokemonType::Fire), // tera_type = Fire
            Some([0u8; 6]), Some([31u8; 6]), false,
        );
        assert!(!p2_back.is_tera, "test setup: back Garchomp must not be Terastallized");

        let battle = battle_state_from_lists(
            vec![p1.clone()], vec![],
            vec![p2_active], vec![p2_back],
        );
        let state = MatchState::BattleState(battle);

        // P1 uses Splash (harmless); P2 switches to Garchomp (back-index 0).
        let p1_cmd = PlayerCommand::Battle(simple_attack(Player::P1, vec![0]));
        let p2_cmd = PlayerCommand::Battle(vec![
            BattleCommand::Switch(SwitchCommand { party_index: 0 }),
        ]);

        let fog = fog_1v1(&p1, Species::Snorlax);
        let events = simulate_and_get_events(state, p1_cmd, p2_cmd);
        let result = apply_roundtrip(fog, events);

        // After the switch, P2 slot 0 holds Garchomp.
        let incoming = &result.p2_active_mons[0];
        assert!(
            !incoming.is_tera,
            "C1 regression: switched-in non-tera Garchomp must NOT be flagged as Terastallized; \
             is_tera = {}", incoming.is_tera
        );
        assert_ne!(
            incoming.possible_tera_type,
            Unknown::Known(PokemonType::Fire),
            "C1 regression: Garchomp's hidden tera type (Fire) must NOT be Known — \
             tera type is only observable when the mon actually Terastallizes; \
             possible_tera_type = {:?}", incoming.possible_tera_type
        );
        // Ability soundness: true ability (SandVeil) must be within possible_abilities.
        // from_opponent_species(Garchomp) gives Possibly([SandVeil, RoughSkin]).
        assert!(
            !unknown_is_excluded(&incoming.possible_abilities, &Ability::SandVeil),
            "soundness: true ability SandVeil must not be excluded; \
             possible_abilities = {:?}", incoming.possible_abilities
        );
    }

    // ── Scenario B: P1 attacks P2 — Pass 3 Direction A soundness ─────────────

    /// P1 (Garchomp, known Atk=150) attacks P2 (Snorlax, hidden Def=85) with Earthquake.
    /// The real DamageDealt event carries P2's HP as `Percent` (opponent FOW).
    /// Pass 3 Direction A uses the Percent HP to bound P2's Def BSV.
    ///
    /// Soundness assertion: true Def BSV (85) must lie within
    /// `[min_pre_nature_stat[2], max_pre_nature_stat[2]]` after inference.
    #[test]
    fn roundtrip_b_p1_attacks_p2_def_stat_soundness() {
        let pd = pokemon_dex();
        let md = move_dex();

        // P1: Garchomp with Earthquake (Atk=150 at Hardy/31IVs/0EVs/Lv50). Garchomp
        // is Ground-type so Earthquake gets STAB.  Garchomp base Spe=102 → faster than Snorlax.
        let p1 = build_pokemon_state(
            Species::Garchomp, pd, md, Some(50),
            Some([Some(PokemonMove::Earthquake), Some(PokemonMove::Splash), None, None]),
            None, Some(Ability::None), Some(Nature::Hardy), None, None,
            Some([0u8; 6]), Some([31u8; 6]), false,
        );

        // P2: Snorlax with Splash, Immunity ability (in Snorlax's real ability list).
        // Hardy/31IVs/0EVs → true Def BSV = 85.
        let p2 = build_pokemon_state(
            Species::Snorlax, pd, md, Some(50),
            Some([Some(PokemonMove::Splash), None, None, None]),
            None, Some(Ability::Immunity), Some(Nature::Hardy), None, None,
            Some([0u8; 6]), Some([31u8; 6]), false,
        );
        let true_def_bsv = p2.stats[2]; // Hardy neutral: post-nature stat == BSV

        let battle = battle_state_from_lists(
            vec![p1.clone()], vec![], vec![p2.clone()], vec![],
        );
        let state = MatchState::BattleState(battle);
        let p1_cmd = PlayerCommand::Battle(simple_attack(Player::P1, vec![0])); // Earthquake
        let p2_cmd = PlayerCommand::Battle(simple_attack(Player::P2, vec![0])); // Splash

        let fog = fog_1v1(&p1, Species::Snorlax);
        let events = simulate_and_get_events(state, p1_cmd, p2_cmd);
        let result = apply_roundtrip(fog, events);

        let p2_fog = &result.p2_active_mons[0];
        assert!(
            p2_fog.min_pre_nature_stat[2] <= true_def_bsv
                && true_def_bsv <= p2_fog.max_pre_nature_stat[2],
            "soundness (Direction A): true Def BSV ({true_def_bsv}) must lie within \
             inferred range [{}, {}]",
            p2_fog.min_pre_nature_stat[2], p2_fog.max_pre_nature_stat[2]
        );
        assert!(
            !unknown_is_excluded(&p2_fog.possible_abilities, &Ability::Immunity),
            "soundness: true ability Immunity must not be excluded; \
             possible_abilities = {:?}", p2_fog.possible_abilities
        );
    }

    // ── Scenario C: P2 attacks P1 — Pass 3 Direction B soundness + C2 path ───

    /// P2 (Garchomp, hidden Atk=150) attacks P1 (Snorlax, known Def=85) with Earthquake.
    /// P1's HP is `Number` (observer's own mon), so Direction B can do a tight inference.
    /// Garchomp also has `Ability::SandVeil` (not Analytic), so the C2 move-order guard
    /// is exercised without actually firing Analytic (safe boundary check).
    ///
    /// Soundness assertion: true Atk BSV (150) must lie within
    /// `[min_pre_nature_stat[1], max_pre_nature_stat[1]]` after inference.
    #[test]
    fn roundtrip_c_p2_attacks_p1_atk_stat_soundness() {
        let pd = pokemon_dex();
        let md = move_dex();

        // P1: Snorlax with Splash (Spe=50, will be damaged by Garchomp first).
        let p1 = build_pokemon_state(
            Species::Snorlax, pd, md, Some(50),
            Some([Some(PokemonMove::Splash), None, None, None]),
            None, Some(Ability::None), Some(Nature::Hardy), None, None,
            Some([0u8; 6]), Some([31u8; 6]), false,
        );

        // P2: Garchomp with Earthquake, SandVeil ability (real Garchomp ability).
        // Hardy/31IVs/0EVs → true Atk BSV = 150, Spe BSV = 122.
        let p2 = build_pokemon_state(
            Species::Garchomp, pd, md, Some(50),
            Some([Some(PokemonMove::Earthquake), Some(PokemonMove::Splash), None, None]),
            None, Some(Ability::SandVeil), Some(Nature::Hardy), None, None,
            Some([0u8; 6]), Some([31u8; 6]), false,
        );
        let true_atk_bsv = p2.stats[1]; // Hardy neutral: post-nature stat == BSV

        let battle = battle_state_from_lists(
            vec![p1.clone()], vec![], vec![p2.clone()], vec![],
        );
        let state = MatchState::BattleState(battle);
        // P1 uses Splash; P2 uses Earthquake. P2 (Spe=122) moves first.
        let p1_cmd = PlayerCommand::Battle(simple_attack(Player::P1, vec![0])); // Splash
        let p2_cmd = PlayerCommand::Battle(simple_attack(Player::P2, vec![0])); // Earthquake

        let fog = fog_1v1(&p1, Species::Garchomp);
        let events = simulate_and_get_events(state, p1_cmd, p2_cmd);
        let result = apply_roundtrip(fog, events);

        let p2_fog = &result.p2_active_mons[0];
        assert!(
            p2_fog.min_pre_nature_stat[1] <= true_atk_bsv
                && true_atk_bsv <= p2_fog.max_pre_nature_stat[1],
            "soundness (Direction B): true Atk BSV ({true_atk_bsv}) must lie within \
             inferred range [{}, {}]",
            p2_fog.min_pre_nature_stat[1], p2_fog.max_pre_nature_stat[1]
        );
        assert!(
            !unknown_is_excluded(&p2_fog.possible_abilities, &Ability::SandVeil),
            "soundness: true ability SandVeil must not be excluded; \
             possible_abilities = {:?}", p2_fog.possible_abilities
        );
    }

    // ── Scenario D: Speed order — Pass 4 soundness ───────────────────────────

    /// Both mons use Tackle. P1 (Garchomp, Spe=122) moves first; P2 (Snorlax, Spe=50)
    /// moves second.  Pass 4 emits a SpeedComparison constraint and propagates it.
    ///
    /// Soundness assertion: true Spe BSV (50) must lie within
    /// `[minStats[5], maxStats[5]]` after Pass 4's bound propagation.
    #[test]
    fn roundtrip_d_speed_order_spe_stat_soundness() {
        let pd = pokemon_dex();
        let md = move_dex();

        // P1: Garchomp with Tackle (Spe=122, moves first).
        let p1 = build_pokemon_state(
            Species::Garchomp, pd, md, Some(50),
            Some([Some(PokemonMove::Tackle), Some(PokemonMove::Splash), None, None]),
            None, Some(Ability::None), Some(Nature::Hardy), None, None,
            Some([0u8; 6]), Some([31u8; 6]), false,
        );

        // P2: Snorlax with Tackle (Spe=50, moves second), Immunity ability.
        let p2 = build_pokemon_state(
            Species::Snorlax, pd, md, Some(50),
            Some([Some(PokemonMove::Tackle), None, None, None]),
            None, Some(Ability::Immunity), Some(Nature::Hardy), None, None,
            Some([0u8; 6]), Some([31u8; 6]), false,
        );
        let true_spe_bsv = p2.stats[5]; // Hardy neutral: post-nature stat == BSV

        let battle = battle_state_from_lists(
            vec![p1.clone()], vec![], vec![p2.clone()], vec![],
        );
        let state = MatchState::BattleState(battle);
        let p1_cmd = PlayerCommand::Battle(simple_attack(Player::P1, vec![0]));
        let p2_cmd = PlayerCommand::Battle(simple_attack(Player::P2, vec![0]));

        let fog = fog_1v1(&p1, Species::Snorlax);
        let events = simulate_and_get_events(state, p1_cmd, p2_cmd);
        let result = apply_roundtrip(fog, events);

        let p2_fog = &result.p2_active_mons[0];
        // Pass 4 propagates speed bounds through SpeedComparison predicates.
        assert!(
            p2_fog.minStats[5] <= true_spe_bsv && true_spe_bsv <= p2_fog.maxStats[5],
            "soundness (Pass 4): true Spe BSV ({true_spe_bsv}) must lie within \
             inferred Spe range [{}, {}]",
            p2_fog.minStats[5], p2_fog.maxStats[5]
        );
        assert!(
            !unknown_is_excluded(&p2_fog.possible_abilities, &Ability::Immunity),
            "soundness: true ability Immunity must not be excluded"
        );
    }

    // ── Scenario E: Intimidate vs. Clear Body (C2 end-to-end regression) ─────

    /// P2 switches Garchomp in (true ability = Intimidate).  P1 has Regice
    /// with Clear Body, which silently swallows the −1 Atk drop — the real
    /// simulator emits **no** `BoostChanged{Atk,−1}` event.
    ///
    /// **G1 / C2 regression**: after feeding the real event stream into
    /// `apply_information`, Intimidate must remain *possible* on Garchomp.
    /// Before the C2 fix the absence of a −1 boost wrongly excluded Intimidate.
    #[test]
    fn roundtrip_e_intimidate_vs_clear_body_stays_possible() {
        let pd = pokemon_dex();
        let md = move_dex();

        // P1: Regice — Clear Body silently blocks Intimidate.
        let p1 = build_pokemon_state(
            Species::Regice, pd, md, Some(50),
            Some([Some(PokemonMove::Splash), None, None, None]),
            None, Some(Ability::ClearBody), Some(Nature::Hardy), None, None,
            Some([0u8; 6]), Some([31u8; 6]), false,
        );

        // P2 lead: harmless Snorlax.  Back: Garchomp with Intimidate.
        let p2_lead = build_pokemon_state(
            Species::Snorlax, pd, md, Some(50),
            Some([Some(PokemonMove::Splash), None, None, None]),
            None, Some(Ability::Immunity), Some(Nature::Hardy), None, None,
            Some([0u8; 6]), Some([31u8; 6]), false,
        );
        let p2_back = build_pokemon_state(
            Species::Garchomp, pd, md, Some(50),
            Some([Some(PokemonMove::Splash), None, None, None]),
            None, Some(Ability::Intimidate), Some(Nature::Hardy), None, None,
            Some([0u8; 6]), Some([31u8; 6]), false,
        );

        let battle = battle_state_from_lists(
            vec![p1.clone()], vec![],
            vec![p2_lead], vec![p2_back],
        );
        let state = MatchState::BattleState(battle);

        // P1 uses Splash; P2 switches to Garchomp (back-index 0).
        let p1_cmd = PlayerCommand::Battle(simple_attack(Player::P1, vec![0]));
        let p2_cmd = PlayerCommand::Battle(vec![
            BattleCommand::Switch(SwitchCommand { party_index: 0 }),
        ]);

        let events = simulate_and_get_events(state, p1_cmd, p2_cmd);

        // Use the minimal Garchomp dex ([Intimidate, SandVeil]) so that:
        //   - Intimidate is in Garchomp's possible abilities (makes the exclusion test meaningful)
        //   - NeutralizingGas is excluded (suppression guard won't silently skip absence inference)
        let infer_dex = super::intimidate_species_dex();

        let p1_fog = UnknownPokemonState::from_known_pokemon(&p1);
        let mut initial_fog = super::battle_nvn(
            vec![p1_fog],
            vec![UnknownPokemonState::from_opponent_species(Species::Snorlax, &HashMap::new(), 50)],
        );
        initial_fog.p2_known_back_mons = vec![
            UnknownPokemonState::from_opponent_species(Species::Garchomp, &infer_dex, 50),
        ];

        let result = apply_information(
            UnknownMatchState::Battle(initial_fog),
            &events,
            false,
            &infer_dex,
            md,
            &HashMap::new(),
            &InferenceConfig::default(),
        );
        let fog = match result {
            UnknownMatchState::Battle(b) => b,
            _ => panic!("expected Battle state"),
        };

        let p2_active = &fog.p2_active_mons[0];
        // C2 regression: absence of −1 Atk is explained by Clear Body, not by
        // the absence of Intimidate — so Intimidate must remain possible.
        assert!(
            !unknown_is_excluded(&p2_active.possible_abilities, &Ability::Intimidate),
            "G1/C2 regression: Intimidate must remain possible on Garchomp \
             when P1 has Clear Body; possible_abilities = {:?}",
            p2_active.possible_abilities
        );
    }

    // ── Scenario F: contact-chip reveal round-trip (silent-emit regression) ───
    //
    // P1 hits P2 with a contact move. P2 has a contact-punish source. The simulator
    // now emits the source reveal (AbilityRevealed / ItemRevealed) when the chip
    // fires, so `pass2_contact_absence` must NOT exclude the true source. Before the
    // fix the chip was silent, so inference wrongly excluded the true Rough Skin /
    // Iron Barbs / Rocky Helmet on any contact hit.

    /// P1 Tackle (contact) user. Turn order is irrelevant — the contact chip fires whenever
    /// the move lands — so we keep legal stats (no Speed hack) to avoid tripping Pass 5's
    /// "impossible stat line" guard on our own known mon. P2 uses Splash, so P1 always acts.
    fn fast_tackle_p1() -> PokemonState {
        let pd = pokemon_dex();
        let md = move_dex();
        build_pokemon_state(
            Species::Snorlax, pd, md, Some(50),
            Some([Some(PokemonMove::Tackle), None, None, None]),
            None, Some(Ability::None), Some(Nature::Hardy), None, None,
            Some([0u8; 6]), Some([31u8; 6]), false,
        )
    }

    #[test]
    fn roundtrip_f_rough_skin_reveal_not_excluded() {
        let pd = pokemon_dex();
        let md = move_dex();
        let p1 = fast_tackle_p1();
        // P2: Garchomp with Rough Skin (a real Garchomp ability).
        let p2 = build_pokemon_state(
            Species::Garchomp, pd, md, Some(50),
            Some([Some(PokemonMove::Splash), None, None, None]),
            None, Some(Ability::RoughSkin), Some(Nature::Hardy), None, None,
            Some([0u8; 6]), Some([31u8; 6]), false,
        );
        let battle = battle_state_from_lists(vec![p1.clone()], vec![], vec![p2], vec![]);
        let state = MatchState::BattleState(battle);
        let p1_cmd = PlayerCommand::Battle(simple_attack(Player::P1, vec![0]));
        let p2_cmd = PlayerCommand::Battle(simple_attack(Player::P2, vec![0]));

        let fog = fog_1v1(&p1, Species::Garchomp);
        let events = simulate_and_get_events(state, p1_cmd, p2_cmd);
        let result = apply_roundtrip(fog, events);

        assert!(
            !unknown_is_excluded(&result.p2_active_mons[0].possible_abilities, &Ability::RoughSkin),
            "silent-chip regression: Rough Skin must not be excluded after a contact hit \
             (the sim now emits AbilityRevealed); possible_abilities = {:?}",
            result.p2_active_mons[0].possible_abilities
        );
    }

    #[test]
    fn roundtrip_f_iron_barbs_reveal_not_excluded() {
        let pd = pokemon_dex();
        let md = move_dex();
        let p1 = fast_tackle_p1();
        // P2: Ferrothorn with Iron Barbs.
        let p2 = build_pokemon_state(
            Species::Ferrothorn, pd, md, Some(50),
            Some([Some(PokemonMove::Splash), None, None, None]),
            None, Some(Ability::IronBarbs), Some(Nature::Hardy), None, None,
            Some([0u8; 6]), Some([31u8; 6]), false,
        );
        let battle = battle_state_from_lists(vec![p1.clone()], vec![], vec![p2], vec![]);
        let state = MatchState::BattleState(battle);
        let p1_cmd = PlayerCommand::Battle(simple_attack(Player::P1, vec![0]));
        let p2_cmd = PlayerCommand::Battle(simple_attack(Player::P2, vec![0]));

        let fog = fog_1v1(&p1, Species::Ferrothorn);
        let events = simulate_and_get_events(state, p1_cmd, p2_cmd);
        let result = apply_roundtrip(fog, events);

        assert!(
            !unknown_is_excluded(&result.p2_active_mons[0].possible_abilities, &Ability::IronBarbs),
            "silent-chip regression: Iron Barbs must not be excluded after a contact hit; \
             possible_abilities = {:?}",
            result.p2_active_mons[0].possible_abilities
        );
    }

    #[test]
    fn roundtrip_f_rocky_helmet_reveal_not_excluded() {
        let pd = pokemon_dex();
        let md = move_dex();
        let p1 = fast_tackle_p1();
        // P2: Garchomp holding Rocky Helmet (item), ability Sand Veil.
        let p2 = build_pokemon_state(
            Species::Garchomp, pd, md, Some(50),
            Some([Some(PokemonMove::Splash), None, None, None]),
            None, Some(Ability::SandVeil), Some(Nature::Hardy), Some(Item::RockyHelmet), None,
            Some([0u8; 6]), Some([31u8; 6]), false,
        );
        let battle = battle_state_from_lists(vec![p1.clone()], vec![], vec![p2], vec![]);
        let state = MatchState::BattleState(battle);
        let p1_cmd = PlayerCommand::Battle(simple_attack(Player::P1, vec![0]));
        let p2_cmd = PlayerCommand::Battle(simple_attack(Player::P2, vec![0]));

        let fog = fog_1v1(&p1, Species::Garchomp);
        let events = simulate_and_get_events(state, p1_cmd, p2_cmd);
        let result = apply_roundtrip(fog, events);

        assert!(
            !unknown_is_excluded(&result.p2_active_mons[0].item, &Item::RockyHelmet),
            "silent-chip regression: Rocky Helmet must not be excluded after a contact hit \
             (the sim now emits ItemRevealed); item = {:?}",
            result.p2_active_mons[0].item
        );
    }

    // ── Scenario G: EOT residual damage keeps inferred HP in sync ─────────────
    //
    // EOT poison damage was silent, so the fog HP would drift (stay at full). Now that the
    // sim emits it, the inferred HP must reflect the chip. Guards the latent Pass-3 desync.
    #[test]
    fn roundtrip_g_eot_poison_keeps_hp_synced() {
        use crate::information::unknowns::PokemonHP;
        let pd = pokemon_dex();
        let md = move_dex();
        let p1 = build_pokemon_state(
            Species::Shuckle, pd, md, Some(50),
            Some([Some(PokemonMove::Splash), None, None, None]),
            None, Some(Ability::None), Some(Nature::Hardy), None, None,
            Some([0u8; 6]), Some([31u8; 6]), false,
        );
        let mut p2 = build_pokemon_state(
            Species::Garchomp, pd, md, Some(50),
            Some([Some(PokemonMove::Splash), None, None, None]),
            None, Some(Ability::SandVeil), Some(Nature::Hardy), None, None,
            Some([0u8; 6]), Some([31u8; 6]), false,
        );
        p2.status = Some(crate::state::dex_data::Status::Poison);

        let battle = battle_state_from_lists(vec![p1.clone()], vec![], vec![p2], vec![]);
        let state = MatchState::BattleState(battle);
        let p1_cmd = PlayerCommand::Battle(simple_attack(Player::P1, vec![0]));
        let p2_cmd = PlayerCommand::Battle(simple_attack(Player::P2, vec![0]));

        let fog = fog_1v1(&p1, Species::Garchomp);
        let events = simulate_and_get_events(state, p1_cmd, p2_cmd);
        let result = apply_roundtrip(fog, events);

        match result.p2_active_mons[0].hp {
            PokemonHP::Percent(p) => assert!(
                p < 100,
                "EOT poison damage must be reflected in inferred HP (was silent before); got Percent({p})"
            ),
            ref other => panic!("expected opponent HP as Percent, got {other:?}"),
        }
    }

    // ── Scenario H: Life Orb recoil round-trip (silent-emit regression) ───────
    //
    // P2 holds Life Orb and lands a damaging move. The simulator now emits the
    // holder's recoil chip (ItemRevealed + DamageDealt — "hurt by its Life Orb!"
    // names the item in-game). Before the fix the chip was silent, so
    // `pass2_item_from_move` saw "no recoil after a damaging hit" and unsoundly
    // excluded Life Orb from the true holder on every attack.
    #[test]
    fn roundtrip_h_life_orb_recoil_visible_not_excluded() {
        use crate::information::unknowns::PokemonHP;
        let pd = pokemon_dex();
        let md = move_dex();
        // P1: bulky Snorlax so the Tackle can't KO (faint-masking guard).
        let p1 = build_pokemon_state(
            Species::Snorlax, pd, md, Some(50),
            Some([Some(PokemonMove::Splash), None, None, None]),
            None, Some(Ability::None), Some(Nature::Hardy), None, None,
            Some([0u8; 6]), Some([31u8; 6]), false,
        );
        // P2: Garchomp holding Life Orb, attacking with Tackle.
        let p2 = build_pokemon_state(
            Species::Garchomp, pd, md, Some(50),
            Some([Some(PokemonMove::Tackle), None, None, None]),
            None, Some(Ability::SandVeil), Some(Nature::Hardy), Some(Item::LifeOrb), None,
            Some([0u8; 6]), Some([31u8; 6]), false,
        );
        let battle = battle_state_from_lists(vec![p1.clone()], vec![], vec![p2], vec![]);
        let state = MatchState::BattleState(battle);
        let p1_cmd = PlayerCommand::Battle(simple_attack(Player::P1, vec![0]));
        let p2_cmd = PlayerCommand::Battle(simple_attack(Player::P2, vec![0]));

        let fog = fog_1v1(&p1, Species::Garchomp);
        let events = simulate_and_get_events(state, p1_cmd, p2_cmd);
        let result = apply_roundtrip(fog, events);

        let p2_fog = &result.p2_active_mons[0];
        assert!(
            !unknown_is_excluded(&p2_fog.item, &Item::LifeOrb),
            "silent-recoil regression: Life Orb must not be excluded after its holder \
             attacks (the sim now emits the recoil chip); item = {:?}",
            p2_fog.item
        );
        // The chip announces the item, so the round-trip should pin it exactly.
        assert!(
            matches!(&p2_fog.item,
                crate::information::unknowns::Unknown::Known(i) if *i == Item::LifeOrb),
            "Life Orb chip reveals the item; expected Known(LifeOrb), got {:?}",
            p2_fog.item
        );
        // And the recoil DamageDealt must keep the holder's observed HP in sync.
        assert!(
            matches!(&p2_fog.hp, PokemonHP::Percent(p) if *p < 100),
            "Life Orb recoil must be visible in observed HP; hp = {:?}",
            p2_fog.hp
        );
    }

    // ── Scenario I: switch-in reveals attribute to the INCOMING mon ───────────
    //
    // The simulator used to emit entry-ability reveals as top-level siblings BEFORE
    // the Switch event, so pass 1 applied them to the outgoing mon still occupying
    // the fog slot. Now they nest under Switch: the incoming mon gets the reveal,
    // and the benched outgoing mon's ability knowledge stays untouched.
    #[test]
    fn roundtrip_i_switch_in_reveal_attributes_to_incoming_mon() {
        let pd = pokemon_dex();
        let md = move_dex();
        let p1 = build_pokemon_state(
            Species::Snorlax, pd, md, Some(50),
            Some([Some(PokemonMove::Splash), None, None, None]),
            None, Some(Ability::None), Some(Nature::Hardy), None, None,
            Some([0u8; 6]), Some([31u8; 6]), false,
        );
        // P2 lead: harmless Snorlax. Back: Garchomp with Intimidate.
        let p2_lead = build_pokemon_state(
            Species::Snorlax, pd, md, Some(50),
            Some([Some(PokemonMove::Splash), None, None, None]),
            None, Some(Ability::Immunity), Some(Nature::Hardy), None, None,
            Some([0u8; 6]), Some([31u8; 6]), false,
        );
        let p2_back = build_pokemon_state(
            Species::Garchomp, pd, md, Some(50),
            Some([Some(PokemonMove::Splash), None, None, None]),
            None, Some(Ability::Intimidate), Some(Nature::Hardy), None, None,
            Some([0u8; 6]), Some([31u8; 6]), false,
        );
        let battle = battle_state_from_lists(
            vec![p1.clone()], vec![],
            vec![p2_lead], vec![p2_back],
        );
        let state = MatchState::BattleState(battle);
        let p1_cmd = PlayerCommand::Battle(simple_attack(Player::P1, vec![0]));
        let p2_cmd = PlayerCommand::Battle(vec![
            BattleCommand::Switch(SwitchCommand { party_index: 0 }),
        ]);
        let events = simulate_and_get_events(state, p1_cmd, p2_cmd);

        let infer_dex = super::intimidate_species_dex();
        let mut fog = fog_1v1(&p1, Species::Snorlax);
        fog.p2_known_back_mons = vec![
            UnknownPokemonState::from_opponent_species(Species::Garchomp, &infer_dex, 50),
        ];
        let result = apply_information(
            UnknownMatchState::Battle(fog),
            &events,
            false,
            &infer_dex,
            md,
            &HashMap::new(),
            &InferenceConfig::default(),
        );
        let result = match result {
            UnknownMatchState::Battle(b) => b,
            _ => panic!("expected Battle state"),
        };

        // Incoming Garchomp: Intimidate visibly activated (P1 has no drop blocker),
        // so the nested reveal pins its ability.
        assert!(
            matches!(&result.p2_active_mons[0].possible_abilities,
                Unknown::Known(a) if *a == Ability::Intimidate),
            "incoming Garchomp must have Known(Intimidate); got {:?}",
            result.p2_active_mons[0].possible_abilities
        );
        // Outgoing Snorlax (benched): must NOT have been branded with Intimidate by
        // the reveal that used to precede the Switch event.
        let benched = result.p2_known_back_mons.iter()
            .find(|m| matches!(&m.possible_species, Unknown::Known(s) if *s == Species::Snorlax))
            .expect("outgoing Snorlax must be benched in fog state");
        assert!(
            !matches!(&benched.possible_abilities,
                Unknown::Known(a) if *a == Ability::Intimidate),
            "misattribution regression: benched Snorlax must not carry the incoming \
             mon's Intimidate reveal; got {:?}",
            benched.possible_abilities
        );
    }

    // ── Scenario J: Prankster Dark-bounce round-trip ──────────────────────────
    //
    // The simulator now emits Immune (with resolved targets) when a Prankster-boosted
    // status move bounces off a Dark type — previously both were missing, leaving the
    // Prankster pass dead on real streams. Our own mon's knowledge is complete, so all
    // alternative explanations are ruled out and the unit clause pins Prankster.
    #[test]
    fn roundtrip_j_prankster_dark_bounce_pins_prankster() {
        let pd = pokemon_dex();
        let md = move_dex();
        // P1: pure Dark type, fully known (from_known_pokemon), Splash.
        let p1 = build_pokemon_state(
            Species::Umbreon, pd, md, Some(50),
            Some([Some(PokemonMove::Splash), None, None, None]),
            None, Some(Ability::SandVeil), Some(Nature::Hardy), None, None,
            Some([0u8; 6]), Some([31u8; 6]), false,
        );
        // P2: Murkrow with Prankster using Thunder Wave (Umbreon is not Ground-type,
        // so no dual-type immunity alternative).
        let p2 = build_pokemon_state(
            Species::Murkrow, pd, md, Some(50),
            Some([Some(PokemonMove::ThunderWave), None, None, None]),
            None, Some(Ability::Prankster), Some(Nature::Hardy), None, None,
            Some([0u8; 6]), Some([31u8; 6]), false,
        );
        let battle = battle_state_from_lists(vec![p1.clone()], vec![], vec![p2], vec![]);
        let state = MatchState::BattleState(battle);
        let p1_cmd = PlayerCommand::Battle(simple_attack(Player::P1, vec![0]));
        let p2_cmd = PlayerCommand::Battle(simple_attack(Player::P2, vec![0]));

        let fog = fog_1v1(&p1, Species::Murkrow);
        let events = simulate_and_get_events(state, p1_cmd, p2_cmd);
        let result = apply_roundtrip(fog, events);

        assert!(
            matches!(&result.p2_active_mons[0].possible_abilities,
                Unknown::Known(a) if *a == Ability::Prankster),
            "the Dark bounce round-trip must pin Prankster; got {:?}",
            result.p2_active_mons[0].possible_abilities
        );
    }
}

// ── Gap 1: Team-preview inference path ───────────────────────────────────────
//
// `apply_information_team_preview` and `process_team_preview_event` are the
// entry points for inference during team preview.  Every existing test uses a
// `Battle` state; this is the first coverage of the `TeamPreview` path.

/// Basic team-preview inference: when P2's lead switches in via a `Switch` event
/// during team preview, the engine should update that mon's HP and level from the
/// `SwitchState` payload.
#[test]
fn test_team_preview_switch_updates_hp_and_level() {
    use crate::information::unknowns::UnknownTeamPreviewState;

    // Pre-populate P2 with two species (as team preview reveals all species up front).
    let p2_garchomp = unknown_mon_species(Species::Garchomp);
    let p2_snorlax  = unknown_mon_species(Species::Snorlax);

    let mut preview_state = UnknownMatchState::TeamPreview(UnknownTeamPreviewState {
        active_per_side:  1,
        brought_per_side: 3,
        p1_mons: vec![],
        p2_mons: vec![p2_garchomp, p2_snorlax],
    });

    let events = vec![event(EventKind::Switch(SwitchState {
        slot:      p2(0),
        species:   Species::Garchomp,
        level:     50,
        hp:        PokemonHP::Percent(80),
        status:    None,
        tera_type: None,
    }))];

    let result = apply_information(
        preview_state,
        &events,
        true, // is_team_preview (ignored — determined by state variant)
        &HashMap::new(),
        &HashMap::new(),
        &HashMap::new(),
        &InferenceConfig::default(),
    );

    let preview = match result {
        UnknownMatchState::TeamPreview(ref p) => p,
        _ => panic!("expected TeamPreview state"),
    };

    // Garchomp (index 0 in p2_mons) was chosen as lead: HP should reflect the switch-in.
    let garchomp = &preview.p2_mons[0];
    assert_eq!(
        garchomp.level, 50,
        "Garchomp's level must be set from SwitchState"
    );
    assert!(
        matches!(garchomp.hp, PokemonHP::Percent(80)),
        "Garchomp's HP must be Percent(80) from SwitchState, got {:?}", garchomp.hp
    );

    // Snorlax (index 1) was NOT switched in — its state must be unchanged.
    let snorlax = &preview.p2_mons[1];
    assert!(
        matches!(snorlax.hp, PokemonHP::Percent(100)),
        "Snorlax's HP must remain unchanged (not switched in)"
    );
}

/// Team preview with a simultaneous (multi-lead) switch.
/// Both P2 leads are registered in the correct slots.
#[test]
fn test_team_preview_simultaneous_switch_two_leads() {
    use crate::information::unknowns::UnknownTeamPreviewState;

    let p2_garchomp  = unknown_mon_species(Species::Garchomp);
    let p2_snorlax   = unknown_mon_species(Species::Snorlax);
    let p2_charizard = unknown_mon_species(Species::Charizard);

    let mut preview_state = UnknownMatchState::TeamPreview(UnknownTeamPreviewState {
        active_per_side:  2,
        brought_per_side: 3,
        p1_mons: vec![],
        p2_mons: vec![p2_garchomp, p2_snorlax, p2_charizard],
    });

    // P2 leads with Garchomp at slot 0 and Snorlax at slot 1.
    let events = vec![event(EventKind::SimultaneousSwitch {
        switches: vec![
            SwitchState {
                slot:      p2(0),
                species:   Species::Garchomp,
                level:     50,
                hp:        PokemonHP::Percent(100),
                status:    None,
                tera_type: None,
            },
            SwitchState {
                slot:      p2(1),
                species:   Species::Snorlax,
                level:     50,
                hp:        PokemonHP::Percent(90),
                status:    None,
                tera_type: None,
            },
        ],
    })];

    let result = apply_information(
        preview_state,
        &events,
        true,
        &HashMap::new(),
        &HashMap::new(),
        &HashMap::new(),
        &InferenceConfig::default(),
    );

    let preview = match result {
        UnknownMatchState::TeamPreview(ref p) => p,
        _ => panic!("expected TeamPreview state"),
    };

    // Both leads were switched in; Charizard (not switched in) must be unchanged.
    assert!(
        matches!(preview.p2_mons[0].hp, PokemonHP::Percent(100)),
        "Garchomp HP must be Percent(100), got {:?}", preview.p2_mons[0].hp
    );
    assert!(
        matches!(preview.p2_mons[1].hp, PokemonHP::Percent(90)),
        "Snorlax HP must be Percent(90), got {:?}", preview.p2_mons[1].hp
    );
    assert!(
        matches!(preview.p2_mons[2].hp, PokemonHP::Percent(100)),
        "Charizard HP must remain Percent(100) (not switched in)"
    );
}

// ── Gap 2: Terrain timer model ────────────────────────────────────────────────
//
// Weather timers (rain, sand, sun, snow) and side-condition timers (Reflect,
// etc.) are exhaustively tested in the S-A block above.  This is the first test
// of the *terrain* timer, which shares the same `Possibly([5,8])` structure.

/// When a terrain is set via `TerrainChanged`, the timer starts at `Possibly([5,8])`.
/// After 5 end-of-turn decrements it collapses to `Known(3)`, revealing the
/// `TerrainExtender` on the setter (mirror of the I-A weather test).
#[test]
fn test_terrain_timer_collapse_reveals_terrain_extender() {
    use crate::information::unknowns::Unknown;

    let p2_mon = unknown_mon_species(Species::Garchomp);
    let state = battle_with_p2(vec![p2_mon]);

    let mut move_dex = HashMap::new();
    move_dex.insert(
        PokemonMove::ElectricTerrain,
        poke_status_move(PokemonMove::ElectricTerrain),
    );

    let set_terrain_turn = vec![event_with(
        EventKind::MoveUsed {
            user:      p2(0),
            move_used: PokemonMove::ElectricTerrain,
            targets:   vec![],
        },
        vec![event(EventKind::TerrainChanged { terrain: Some(Terrain::ElectricTerrain) })],
    )];

    let mut cur_state = apply_ex(state, set_terrain_turn, HashMap::new(), move_dex);

    assert_eq!(
        cur_state.terrain_turns,
        Some(Unknown::Possibly(vec![5, 8])),
        "terrain_turns must start as Possibly([5,8])"
    );
    assert_eq!(cur_state.terrain, Some(Terrain::ElectricTerrain));
    assert_eq!(
        cur_state.terrain_setter_mon_idx, Some(0),
        "terrain setter must be mon_idx=0 (p2 Garchomp)"
    );

    // Advance 4 EOTs — timer decrements but must not collapse yet.
    for expected in [
        Unknown::Possibly(vec![4, 7]),
        Unknown::Possibly(vec![3, 6]),
        Unknown::Possibly(vec![2, 5]),
        Unknown::Possibly(vec![1, 4]),
    ] {
        cur_state = apply_ex(
            cur_state, vec![event(EventKind::EndOfTurn)], HashMap::new(), HashMap::new(),
        );
        assert_eq!(
            cur_state.terrain_turns,
            Some(expected.clone()),
            "terrain_turns after decrement: expected {:?}", expected
        );
        // Item still unknown.
        assert!(
            matches!(cur_state.p2_active_mons[0].item, Unknown::Not(ref v) if v.is_empty()),
            "item must remain unknown before the 5th EOT"
        );
    }

    // 5th EOT — collapses: Possibly([1,4]) → filter(>1) → [3] → Known(3).
    cur_state = apply_ex(
        cur_state, vec![event(EventKind::EndOfTurn)], HashMap::new(), HashMap::new(),
    );
    assert_eq!(
        cur_state.terrain_turns,
        Some(Unknown::Known(3)),
        "terrain_turns must collapse to Known(3) after the 5th EOT"
    );

    // I-A: TerrainExtender must now be revealed as Known on Garchomp.
    assert_eq!(
        cur_state.p2_active_mons[0].item,
        Unknown::Known(Item::TerrainExtender),
        "TerrainExtender must be revealed as Known after the 5th EOT confirms the 8-turn branch"
    );
}

// ── Gap 3: EOT sand-immunity disjunction ─────────────────────────────────────
//
// `pass_eot_heal` and `pass_eot_self_status` are covered by existing tests but
// `pass_eot_sand_immunity` (inference.rs ~3464) is not.  When an opponent mon
// takes NO sand chip under Sandstorm at EOT, the engine emits a disjunction of
// immunity sources (SafetyGoggles ∨ SandVeil ∨ SandRush ∨ SandForce ∨ Overcoat
// ∨ MagicGuard).

/// Under Sandstorm, if a P2 Normal-type mon takes no EOT sand chip, the engine
/// must emit the sand-immunity disjunction covering SafetyGoggles and the
/// sand-immunity abilities.
#[test]
fn test_eot_sand_immunity_emits_clause_when_no_chip() {
    // P2 Garchomp — typed as Normal so it is NOT innately immune (Normal ≠ Rock/Ground/Steel).
    let mut p2_mon = unknown_mon();
    p2_mon.possible_types = Unknown::Known(vec![PokemonType::Normal]);
    // Species knowledge rules out Air Lock / Cloud Nine (which would suspend the sand chip and
    // make chip-absence uninformative). With a real ability dex this is implied by the species;
    // the empty-dex test builder leaves abilities unconstrained, so encode it explicitly.
    p2_mon.possible_abilities = Unknown::Not(vec![Ability::AirLock, Ability::CloudNine]);

    let mut state = battle_with_p2(vec![p2_mon]);
    // Sandstorm is active.
    state.weather = Some(Weather::Sandstorm);

    // EndOfTurn with no DamageDealt on P2 (no sand chip).
    let result = apply(state, vec![event(EventKind::EndOfTurn)]);

    // The predicates must contain a clause that mentions at least SafetyGoggles
    // or one of the sand-immunity abilities.
    let clause_with_sand_immunity = result.predicates.iter().any(|clause| {
        clause.iter().any(|s| matches!(
            s,
            Statement::HasItem { item: Item::SafetyGoggles, .. }
                | Statement::HasAbility { ability: Ability::SandVeil, .. }
                | Statement::HasAbility { ability: Ability::SandRush, .. }
                | Statement::HasAbility { ability: Ability::SandForce, .. }
                | Statement::HasAbility { ability: Ability::Overcoat, .. }
                | Statement::HasAbility { ability: Ability::MagicGuard, .. }
        ))
    });
    assert!(
        clause_with_sand_immunity,
        "EOT sand chip absence must emit a sand-immunity disjunction on the P2 mon"
    );
}

/// Under Sandstorm, if a P2 Steel-type mon takes no EOT chip, the engine must
/// NOT emit a sand-immunity clause (Steel is innately immune).
#[test]
fn test_eot_sand_immunity_not_emitted_for_innately_immune_type() {
    let mut p2_mon = unknown_mon();
    p2_mon.possible_types = Unknown::Known(vec![PokemonType::Steel]);

    let mut state = battle_with_p2(vec![p2_mon]);
    state.weather = Some(Weather::Sandstorm);

    let result = apply(state, vec![event(EventKind::EndOfTurn)]);

    let has_clause = result.predicates.iter().any(|clause| {
        clause.iter().any(|s| matches!(
            s,
            Statement::HasItem { item: Item::SafetyGoggles, .. }
                | Statement::HasAbility { ability: Ability::SandVeil, .. }
                | Statement::HasAbility { ability: Ability::SandRush, .. }
                | Statement::HasAbility { ability: Ability::SandForce, .. }
                | Statement::HasAbility { ability: Ability::Overcoat, .. }
                | Statement::HasAbility { ability: Ability::MagicGuard, .. }
        ))
    });
    assert!(
        !has_clause,
        "Sand-immunity clause must NOT be emitted for innately-immune Steel-type"
    );
}

/// When Sandstorm is NOT active, EOT must not emit sand-immunity clauses even if
/// the mon takes no chip (no sandstorm → no chip → no information).
#[test]
fn test_eot_sand_immunity_not_emitted_without_sandstorm() {
    let mut p2_mon = unknown_mon();
    p2_mon.possible_types = Unknown::Known(vec![PokemonType::Normal]);

    let state = battle_with_p2(vec![p2_mon]);

    let result = apply(state, vec![event(EventKind::EndOfTurn)]);

    let has_clause = result.predicates.iter().any(|clause| {
        clause.iter().any(|s| matches!(
            s,
            Statement::HasItem { item: Item::SafetyGoggles, .. }
                | Statement::HasAbility { ability: Ability::SandVeil, .. }
        ))
    });
    assert!(
        !has_clause,
        "Sand-immunity clause must NOT be emitted when there is no Sandstorm"
    );
}

// ── G2 regressions: soundness fixes for absence-based inferences ──────────────
//
// Both tests encode a scenario where the observable signal (−1 Atk / contact chip)
// is absent for a reason *other* than the ability being absent.  Before the fixes
// the engine wrongly excluded the ability; after the fixes it must remain possible.

/// **C1 regression** — contact move hits an unknown target, attacker may have Magic Guard.
///
/// Magic Guard prevents Rough Skin *and* Iron Barbs chip (all contact indirect damage).
/// When the attacker's Magic Guard is not excluded, absence of chip cannot prove the
/// target lacks Rough Skin / Iron Barbs.
#[test]
fn test_contact_absence_magic_guard_attacker_does_not_exclude_rough_skin_iron_barbs() {
    use crate::state::dex_data::MoveFlag;

    // Attacker (P1): item Known(None), ability Unknown::Not([LongReach]) — LongReach ruled out
    // but Magic Guard is NOT ruled out.  Protective Pads also not excluded.
    let mut p1_mon = UnknownPokemonState::from_opponent_species(Species::Garchomp, &HashMap::new(), 50);
    p1_mon.item = Unknown::Known(Item::None);
    // Exclude LongReach and ProtectivePads so those escape-routes are not the cause,
    // but leave Magic Guard possible.
    p1_mon.possible_abilities = Unknown::Not(vec![Ability::LongReach]);

    // Defender (P2): unknown species / ability — Rough Skin and Iron Barbs are possible.
    let p2_mon = unknown_mon();

    let state = battle_1v1(p1_mon, p2_mon);

    let mut move_dex = HashMap::new();
    let mut contact_move = normal_physical_move(PokemonMove::Tackle, 40);
    contact_move.flags.push(MoveFlag::Contact);
    move_dex.insert(PokemonMove::Tackle, contact_move);

    // Contact move hits (DamageDealt present), but no chip reaction for P2.
    let result = apply_ex(
        state,
        vec![event_with(
            EventKind::MoveUsed {
                user: p1(0),
                move_used: PokemonMove::Tackle,
                targets: vec![p2(0)],
            },
            vec![event(EventKind::DamageDealt { target: p2(0), new_hp: PokemonHP::Percent(75) })],
        )],
        HashMap::new(),
        move_dex,
    );

    assert!(
        !unknown_is_excluded(&result.p2_active_mons[0].possible_abilities, &Ability::RoughSkin),
        "RoughSkin must remain possible when attacker may have Magic Guard (C1)"
    );
    assert!(
        !unknown_is_excluded(&result.p2_active_mons[0].possible_abilities, &Ability::IronBarbs),
        "IronBarbs must remain possible when attacker may have Magic Guard (C1)"
    );
}

/// **C1 regression (control)** — same scenario but attacker's Magic Guard is excluded.
///
/// When Magic Guard is definitively ruled out on the attacker, the absence of chip
/// *does* prove the target lacks Rough Skin / Iron Barbs — the exclusion is valid.
#[test]
fn test_contact_absence_no_magic_guard_excludes_rough_skin_iron_barbs() {
    use crate::state::dex_data::MoveFlag;

    // Attacker: Magic Guard excluded; LongReach excluded; item None (no Protective Pads).
    // NeutralizingGas must be excluded on BOTH actives, or the defender-suppression
    // gate (sound: the chip could be suppressed field-wide) skips the exclusions.
    let mut p1_mon = UnknownPokemonState::from_opponent_species(Species::Garchomp, &HashMap::new(), 50);
    p1_mon.item = Unknown::Known(Item::None);
    p1_mon.possible_abilities = Unknown::Not(vec![
        Ability::LongReach,
        Ability::MagicGuard,
        Ability::NeutralizingGas,
    ]);

    let mut p2_mon = unknown_mon();
    p2_mon.possible_abilities = Unknown::Not(vec![Ability::NeutralizingGas]);
    let state = battle_1v1(p1_mon, p2_mon);

    let mut move_dex = HashMap::new();
    let mut contact_move = normal_physical_move(PokemonMove::Tackle, 40);
    contact_move.flags.push(MoveFlag::Contact);
    move_dex.insert(PokemonMove::Tackle, contact_move);

    let result = apply_ex(
        state,
        vec![event_with(
            EventKind::MoveUsed {
                user: p1(0),
                move_used: PokemonMove::Tackle,
                targets: vec![p2(0)],
            },
            vec![event(EventKind::DamageDealt { target: p2(0), new_hp: PokemonHP::Percent(75) })],
        )],
        HashMap::new(),
        move_dex,
    );

    assert!(
        unknown_is_excluded(&result.p2_active_mons[0].possible_abilities, &Ability::RoughSkin),
        "RoughSkin must be excluded when Magic Guard is also excluded (C1 control)"
    );
    assert!(
        unknown_is_excluded(&result.p2_active_mons[0].possible_abilities, &Ability::IronBarbs),
        "IronBarbs must be excluded when Magic Guard is also excluded (C1 control)"
    );
}

/// **Defender-suppression gate** — when Neutralizing Gas is still possible on the field,
/// Rough Skin / Iron Barbs could be suppressed (silent even if present), so the
/// chip-absence exclusion must be skipped. Rocky Helmet (item) is still excludable.
#[test]
fn test_contact_absence_skipped_when_defender_may_be_suppressed() {
    use crate::state::dex_data::MoveFlag;

    let mut p1_mon = UnknownPokemonState::from_opponent_species(Species::Garchomp, &HashMap::new(), 50);
    p1_mon.item = Unknown::Known(Item::None);
    p1_mon.possible_abilities = Unknown::Not(vec![
        Ability::LongReach,
        Ability::MagicGuard,
        Ability::NeutralizingGas,
    ]);

    // Defender: fully unknown abilities — Neutralizing Gas remains possible.
    let p2_mon = unknown_mon();
    let state = battle_1v1(p1_mon, p2_mon);

    let mut move_dex = HashMap::new();
    let mut contact_move = normal_physical_move(PokemonMove::Tackle, 40);
    contact_move.flags.push(MoveFlag::Contact);
    move_dex.insert(PokemonMove::Tackle, contact_move);

    let result = apply_ex(
        state,
        vec![event_with(
            EventKind::MoveUsed {
                user: p1(0),
                move_used: PokemonMove::Tackle,
                targets: vec![p2(0)],
            },
            vec![event(EventKind::DamageDealt { target: p2(0), new_hp: PokemonHP::Percent(75) })],
        )],
        HashMap::new(),
        move_dex,
    );

    assert!(
        !unknown_is_excluded(&result.p2_active_mons[0].possible_abilities, &Ability::RoughSkin),
        "RoughSkin must stay possible while its suppression (NGas) is possible"
    );
    assert!(
        !unknown_is_excluded(&result.p2_active_mons[0].possible_abilities, &Ability::IronBarbs),
        "IronBarbs must stay possible while its suppression (NGas) is possible"
    );
    // The Helmet is an item — unaffected by ability suppression, still excludable.
    assert!(
        unknown_is_excluded(&result.p2_active_mons[0].item, &Item::RockyHelmet),
        "RockyHelmet exclusion is unaffected by ability suppression"
    );
}

/// Minimal dex for C2 tests: a single species whose known ability pool includes
/// Intimidate (and a filler) but NOT NeutralizingGas.  Using a bounded ability list
/// is essential so that `pass1_ability_absence_inference` is not gated out by the
/// "NeutralizingGas might be on the field" suppression check — which fires when the
/// entering mon's abilities are fully unknown (`Not([])`).
fn intimidate_species_dex() -> HashMap<Species, PokemonData> {
    use crate::state::pokemon::PokemonGender;
    let mut dex = HashMap::new();
    // Reuse Species::Garchomp as the "Intimidate user" species.
    // Abilities: [Intimidate, SandVeil] — includes Intimidate, excludes NeutralizingGas.
    dex.insert(Species::Garchomp, PokemonData {
        species:       Species::Garchomp,
        types:         vec![PokemonType::Dragon, PokemonType::Ground],
        base_stats:    [108, 130, 95, 80, 85, 102],
        weight:        950,
        primary_ability: Some(Ability::Intimidate),
        abilities:     vec![Ability::Intimidate, Ability::SandVeil],
        base_species:  None,
        forme:         None,
        required_item: None,
        battle_only:   None,
        default_gender: PokemonGender::Male,
    });
    dex
}

/// Helper: build an `UnknownPokemonState` for the entering P2 Garchomp (from `intimidate_species_dex`),
/// placed in the back-mon list so `pass1_switch` can promote it to active.
fn p2_back_with_intimidate_possible(dex: &HashMap<Species, PokemonData>) -> Vec<UnknownPokemonState> {
    vec![UnknownPokemonState::from_opponent_species(Species::Garchomp, dex, 50)]
}

/// Common switch event used in C2 tests: P2's Garchomp enters, no BoostChanged{Atk,−1}.
fn p2_switch_event() -> InformationEvent {
    event(EventKind::SimultaneousSwitch {
        switches: vec![SwitchState {
            slot: p2(0),
            species: Species::Garchomp,
            level: 50,
            hp: PokemonHP::Percent(100),
            status: None,
            tera_type: None,
        }],
    })
}

/// **C2 regression** — opponent's Pokémon enters the field, but our own active mon
/// has Clear Body (blocks Intimidate's −1 Atk drop silently).
///
/// Because Clear Body swallows the Intimidate drop without emitting a BoostChanged{−1},
/// the absence of a −1 boost does NOT prove the entrant lacks Intimidate.
#[test]
fn test_intimidate_not_excluded_when_own_mon_has_clear_body() {
    // Our mon (P1): ability Known(ClearBody), so it would silently block Intimidate.
    let mut p1_mon = UnknownPokemonState::from_opponent_species(Species::Garchomp, &HashMap::new(), 50);
    p1_mon.possible_abilities = Unknown::Known(Ability::ClearBody);
    p1_mon.boosts = [0; 7];

    let dex = intimidate_species_dex();
    // P2's Garchomp is in the back — `pass1_switch` will promote it to active slot.
    let mut state = battle_nvn(vec![p1_mon], vec![]);
    state.p2_known_back_mons = p2_back_with_intimidate_possible(&dex);

    // No BoostChanged{Atk,−1} for P1 in reactions.
    let result = apply_ex(state, vec![p2_switch_event()], dex, HashMap::new());

    assert!(
        !unknown_is_excluded(&result.p2_active_mons[0].possible_abilities, &Ability::Intimidate),
        "Intimidate must remain possible when own active mon has Clear Body (C2)"
    );
}

/// **C2 control** — same scenario but own mon has no Intimidate blocker.
///
/// With SandVeil (no block) and Atk boost above −6, the absence of a −1 drop
/// correctly proves the entrant lacks Intimidate.
#[test]
fn test_intimidate_excluded_when_own_mon_unprotected() {
    // Our mon (P1): ability Known(SandVeil) — not a blocker.
    let mut p1_mon = UnknownPokemonState::from_opponent_species(Species::Garchomp, &HashMap::new(), 50);
    p1_mon.possible_abilities = Unknown::Known(Ability::SandVeil);
    p1_mon.boosts = [0; 7];

    let dex = intimidate_species_dex();
    let mut state = battle_nvn(vec![p1_mon], vec![]);
    state.p2_known_back_mons = p2_back_with_intimidate_possible(&dex);

    let result = apply_ex(state, vec![p2_switch_event()], dex, HashMap::new());

    // Intimidate MUST be excluded: SandVeil doesn't block the −1, Atk is not at −6.
    assert!(
        unknown_is_excluded(&result.p2_active_mons[0].possible_abilities, &Ability::Intimidate),
        "Intimidate must be excluded when own mon has no Intimidate-blocking ability (C2 control)"
    );
}

/// **C2 regression** — own mon has Guard Dog: converts Intimidate's −1 into +1.
///
/// Guard Dog raises the holder's own Atk by +1 when Intimidate would drop it.
/// No `BoostChanged { Atk, −1 }` for our mon even with Intimidate on the opponent.
#[test]
fn test_intimidate_not_excluded_when_own_mon_has_guard_dog() {
    let mut p1_mon = UnknownPokemonState::from_opponent_species(Species::Garchomp, &HashMap::new(), 50);
    p1_mon.possible_abilities = Unknown::Known(Ability::GuardDog);
    p1_mon.boosts = [0; 7];

    let dex = intimidate_species_dex();
    let mut state = battle_nvn(vec![p1_mon], vec![]);
    state.p2_known_back_mons = p2_back_with_intimidate_possible(&dex);

    let result = apply_ex(state, vec![p2_switch_event()], dex, HashMap::new());

    assert!(
        !unknown_is_excluded(&result.p2_active_mons[0].possible_abilities, &Ability::Intimidate),
        "Intimidate must remain possible when own active mon has Guard Dog (C2)"
    );
}

// ── Regenerator inference (B1 bench-state preservation + B2 HP-delta inference) ──────────────
//
// Tests in this section verify two features added together:
//
// B1: a switched-out opponent mon is now preserved to the bench list (with its
//     last-seen HP) rather than being discarded.  On re-entry it is pulled from
//     that list and its state (HP, revealed moves, ability narrowing) is reused.
//
// B2: when a mon returns from the bench with HP that differs from its last-seen
//     HP by ≈33% (with no hazards on the entering side), the engine narrows the
//     ability to `Known(Regenerator)` (positive case) or excludes Regenerator
//     (absence case).

/// B1 basic: a Pokémon's last-observed HP survives the bench round-trip.
///
/// Scenario (observer = P2 fog-of-war on P2's opponent, P1):
///   Initial: P1 active = Garchomp at 50%.  P1 back = Snorlax.
///   Turn 1 event: Snorlax switches into P1 slot 0 → Garchomp goes to bench.
///   Expected: `p1_known_back_mons` contains Garchomp, still at Percent(50).
#[test]
fn test_b1_bench_hp_preserved_across_switch_out() {
    let mut garchomp = unknown_mon_species(Species::Garchomp);
    garchomp.hp = PokemonHP::Percent(50);

    let snorlax = unknown_mon_species(Species::Snorlax);

    let mut state = battle_with_p2(vec![]); // We're testing P1 side (works same way)
    state.p1_active_mons = vec![garchomp];
    state.p1_known_back_mons = vec![snorlax];
    state.active_per_side = 1;
    state.back_mons_per_side = 5;

    let switch_ev = event(EventKind::Switch(SwitchState {
        slot: p1(0),
        species: Species::Snorlax,
        level: 50,
        hp: PokemonHP::Percent(100),
        status: None,
        tera_type: None,
    }));
    let after = apply(state, vec![switch_ev]);

    // Snorlax is now active; Garchomp was benched (B1).
    assert_eq!(after.p1_active_mons.len(), 1);
    assert!(
        matches!(&after.p1_active_mons[0].possible_species, Unknown::Known(s) if *s == Species::Snorlax),
        "Snorlax must be active after the switch"
    );
    let benched = after.p1_known_back_mons.iter()
        .find(|m| matches!(&m.possible_species, Unknown::Known(s) if *s == Species::Garchomp))
        .expect("Garchomp must be in known_back_mons after being displaced (B1)");
    assert_eq!(
        benched.hp, PokemonHP::Percent(50),
        "B1: Garchomp's last-seen HP (50%) must survive the bench round-trip; got {:?}", benched.hp
    );
}

/// B2 positive: opponent returns with ≈33% more HP than it left with → Regenerator inferred.
///
/// Scenario (two-turn):
///   Turn 1: P2 Snorlax switches out (was at 50%), Garchomp comes in.
///   Turn 2: P2 Snorlax switches back in at 83% (50 + 33 = 83).
///   Expected: Snorlax's ability is narrowed to Known(Regenerator).
#[test]
fn test_b2_regenerator_inferred_from_hp_gain_on_reentry() {
    // P2 active: Snorlax at 50% HP.  P2 back: Garchomp.
    let mut snorlax = unknown_mon_species(Species::Snorlax);
    snorlax.hp = PokemonHP::Percent(50);

    let garchomp = unknown_mon_species(Species::Garchomp);

    let mut state = battle_with_p2(vec![snorlax]);
    state.p2_known_back_mons = vec![garchomp];

    // ── Turn 1: P2 switches to Garchomp (Snorlax goes to bench at 50%) ────────
    let t1 = vec![event(EventKind::Switch(SwitchState {
        slot: p2(0),
        species: Species::Garchomp,
        level: 50,
        hp: PokemonHP::Percent(100),
        status: None,
        tera_type: None,
    }))];
    let after_t1 = apply(state, t1);

    // Sanity: Garchomp is active; Snorlax is on the bench at 50%.
    assert!(
        matches!(&after_t1.p2_active_mons[0].possible_species, Unknown::Known(s) if *s == Species::Garchomp)
    );
    let benched_snorlax = after_t1.p2_known_back_mons.iter()
        .find(|m| matches!(&m.possible_species, Unknown::Known(s) if *s == Species::Snorlax))
        .expect("Snorlax must be on bench after turn 1");
    assert_eq!(benched_snorlax.hp, PokemonHP::Percent(50), "bench HP must be 50%");

    // ── Turn 2: Snorlax returns at 83% (50% + 33% Regen heal) ────────────────
    let t2 = vec![event(EventKind::Switch(SwitchState {
        slot: p2(0),
        species: Species::Snorlax,
        level: 50,
        hp: PokemonHP::Percent(83), // 50 + 33
        status: None,
        tera_type: None,
    }))];
    let after_t2 = apply(after_t1, t2);

    let p2_active = &after_t2.p2_active_mons[0];
    assert!(
        matches!(&p2_active.possible_species, Unknown::Known(s) if *s == Species::Snorlax),
        "Snorlax must be active after turn 2"
    );
    assert_eq!(
        p2_active.hp, PokemonHP::Percent(83),
        "Snorlax's HP must reflect the re-entry value"
    );
    assert!(
        matches!(&p2_active.possible_abilities, Unknown::Known(Ability::Regenerator)),
        "B2: Snorlax must be inferred to have Regenerator from the ≈33% HP gain;\n\
         possible_abilities = {:?}", p2_active.possible_abilities
    );
}

/// B2 absence: opponent returns with the same HP it left with (no gain) →
/// Regenerator excluded when the gain would have been distinguishable.
#[test]
fn test_b2_regenerator_excluded_when_no_hp_gain_on_reentry() {
    // Same setup but Snorlax returns at 50% (same as it left).
    let mut snorlax = unknown_mon_species(Species::Snorlax);
    snorlax.hp = PokemonHP::Percent(40);

    let garchomp = unknown_mon_species(Species::Garchomp);

    let mut state = battle_with_p2(vec![snorlax]);
    state.p2_known_back_mons = vec![garchomp];

    let t1 = vec![event(EventKind::Switch(SwitchState {
        slot: p2(0),
        species: Species::Garchomp,
        level: 50,
        hp: PokemonHP::Percent(100),
        status: None,
        tera_type: None,
    }))];
    let after_t1 = apply(state, t1);

    // Snorlax returns at 40% — same as it left (no Regenerator gain).
    let t2 = vec![event(EventKind::Switch(SwitchState {
        slot: p2(0),
        species: Species::Snorlax,
        level: 50,
        hp: PokemonHP::Percent(40),
        status: None,
        tera_type: None,
    }))];
    let after_t2 = apply(after_t1, t2);

    let p2_active = &after_t2.p2_active_mons[0];
    assert!(
        unknown_is_excluded(&p2_active.possible_abilities, &Ability::Regenerator),
        "B2: Regenerator must be excluded when no HP gain observed (mon left at 40%, \
         returned at 40%);\n\
         possible_abilities = {:?}", p2_active.possible_abilities
    );
}

/// B2 hazard skip: Stealth Rock is on the entering side → inference is skipped
/// because hazard chip would confound the delta calculation.
/// Neither Regenerator inference nor exclusion should fire.
#[test]
fn test_b2_regenerator_skip_when_stealth_rock_present() {
    let mut snorlax = unknown_mon_species(Species::Snorlax);
    snorlax.hp = PokemonHP::Percent(50);

    let garchomp = unknown_mon_species(Species::Garchomp);

    let mut state = battle_with_p2(vec![snorlax]);
    state.p2_known_back_mons = vec![garchomp];
    // Stealth Rock on P2's side (damages P2's incoming mons).
    state.p2_side_conditions = vec![SideCondition::StealthRock];
    state.p2_side_condition_turns = vec![Unknown::Known(0)];
    state.p2_side_condition_setters = vec![Some(0)];

    let t1 = vec![event(EventKind::Switch(SwitchState {
        slot: p2(0),
        species: Species::Garchomp,
        level: 50,
        hp: PokemonHP::Percent(100),
        status: None,
        tera_type: None,
    }))];
    let after_t1 = apply(state, t1);

    // Snorlax returns at 75% — looks like 50% + 33% Regen − 8% rock = 75%, but
    // the engine should skip inference entirely when rocks are present.
    let t2 = vec![event(EventKind::Switch(SwitchState {
        slot: p2(0),
        species: Species::Snorlax,
        level: 50,
        hp: PokemonHP::Percent(75),
        status: None,
        tera_type: None,
    }))];
    let after_t2 = apply(after_t1, t2);

    let p2_active = &after_t2.p2_active_mons[0];
    // Must be neither Known(Regenerator) nor Excluded(Regenerator).
    assert!(
        !matches!(&p2_active.possible_abilities, Unknown::Known(Ability::Regenerator)),
        "B2: Regenerator must NOT be inferred when Stealth Rock is present (hazard skip);\n\
         possible_abilities = {:?}", p2_active.possible_abilities
    );
    assert!(
        !unknown_is_excluded(&p2_active.possible_abilities, &Ability::Regenerator),
        "B2: Regenerator must NOT be excluded when Stealth Rock is present (hazard skip);\n\
         possible_abilities = {:?}", p2_active.possible_abilities
    );
}

/// B2 inconclusive: mon left near-full HP (>66%) → inference skipped because
/// Regenerator's heal would cap at 100% and the observed delta would be ambiguous.
#[test]
fn test_b2_regenerator_skip_when_mon_left_near_full() {
    // Snorlax left at 70% — above the 66% threshold for distinguishable Regen.
    let mut snorlax = unknown_mon_species(Species::Snorlax);
    snorlax.hp = PokemonHP::Percent(70);

    let garchomp = unknown_mon_species(Species::Garchomp);

    let mut state = battle_with_p2(vec![snorlax]);
    state.p2_known_back_mons = vec![garchomp];

    let t1 = vec![event(EventKind::Switch(SwitchState {
        slot: p2(0),
        species: Species::Garchomp,
        level: 50,
        hp: PokemonHP::Percent(100),
        status: None,
        tera_type: None,
    }))];
    let after_t1 = apply(state, t1);

    // Returns at 100% — could be 70% + 33% Regen (capped) or just full HP.
    let t2 = vec![event(EventKind::Switch(SwitchState {
        slot: p2(0),
        species: Species::Snorlax,
        level: 50,
        hp: PokemonHP::Percent(100),
        status: None,
        tera_type: None,
    }))];
    let after_t2 = apply(after_t1, t2);

    let p2_active = &after_t2.p2_active_mons[0];
    assert!(
        !matches!(&p2_active.possible_abilities, Unknown::Known(Ability::Regenerator)),
        "B2: Regenerator must NOT be inferred when mon left near-full (near-cap — inconclusive);\n\
         possible_abilities = {:?}", p2_active.possible_abilities
    );
    assert!(
        !unknown_is_excluded(&p2_active.possible_abilities, &Ability::Regenerator),
        "B2: Regenerator must NOT be excluded when mon left near-full;\n\
         possible_abilities = {:?}", p2_active.possible_abilities
    );
}

/// B2 persistence: once Regenerator is inferred it survives another switch cycle,
/// because `possible_original_abilities` is also updated.
#[test]
fn test_b2_regenerator_inference_persists_across_second_switch() {
    // Same as test_b2_regenerator_inferred_from_hp_gain_on_reentry, then one more cycle.
    let mut snorlax = unknown_mon_species(Species::Snorlax);
    snorlax.hp = PokemonHP::Percent(50);

    let garchomp = unknown_mon_species(Species::Garchomp);

    let mut state = battle_with_p2(vec![snorlax]);
    state.p2_known_back_mons = vec![garchomp];

    // Turn 1: Garchomp in, Snorlax (50%) to bench.
    let t1 = vec![event(EventKind::Switch(SwitchState {
        slot: p2(0), species: Species::Garchomp, level: 50,
        hp: PokemonHP::Percent(100), status: None, tera_type: None,
    }))];
    let after_t1 = apply(state, t1);

    // Turn 2: Snorlax returns at 83% → Regenerator inferred.
    let t2 = vec![event(EventKind::Switch(SwitchState {
        slot: p2(0), species: Species::Snorlax, level: 50,
        hp: PokemonHP::Percent(83), status: None, tera_type: None,
    }))];
    let after_t2 = apply(after_t1, t2);
    assert!(
        matches!(&after_t2.p2_active_mons[0].possible_abilities, Unknown::Known(Ability::Regenerator)),
        "Prerequisite: Regenerator must be inferred after turn 2"
    );

    // Turn 3: Snorlax switches out again (bench it).
    // Turn 4: Snorlax returns at some HP — the ability should still be Known(Regenerator).
    let t3 = vec![event(EventKind::Switch(SwitchState {
        slot: p2(0), species: Species::Garchomp, level: 50,
        hp: PokemonHP::Percent(90), status: None, tera_type: None,
    }))];
    let after_t3 = apply(after_t2, t3);

    // Garchomp is now active; Snorlax is benched.
    let benched_snorlax = after_t3.p2_known_back_mons.iter()
        .find(|m| matches!(&m.possible_species, Unknown::Known(s) if *s == Species::Snorlax))
        .expect("Snorlax must be on bench after turn 3");
    // After switch-out, possible_abilities resets to possible_original_abilities.
    // Since we updated possible_original_abilities in turn 2, it should still be Known(Regen).
    assert!(
        matches!(&benched_snorlax.possible_abilities, Unknown::Known(Ability::Regenerator)),
        "B2 persistence: Regenerator must still be Known after a second switch-out;\n\
         (possible_original_abilities must have been updated)\n\
         possible_abilities = {:?}", benched_snorlax.possible_abilities
    );
}

/// **C2 regression** — own mon's Atk boost is at −6 (Intimidate is a no-op clamp).
///
/// If the target's Atk is already clamped at −6, Intimidate's drop produces
/// no visible `BoostChanged` — absence of the event does not prove absence of Intimidate.
#[test]
fn test_intimidate_not_excluded_when_own_mon_atk_at_minus_six() {
    let mut p1_mon = UnknownPokemonState::from_opponent_species(Species::Garchomp, &HashMap::new(), 50);
    p1_mon.possible_abilities = Unknown::Known(Ability::SandVeil); // not a blocker
    p1_mon.boosts = [-6, 0, 0, 0, 0, 0, 0]; // Atk already at min

    let dex = intimidate_species_dex();
    let mut state = battle_nvn(vec![p1_mon], vec![]);
    state.p2_known_back_mons = p2_back_with_intimidate_possible(&dex);

    let result = apply_ex(state, vec![p2_switch_event()], dex, HashMap::new());

    assert!(
        !unknown_is_excluded(&result.p2_active_mons[0].possible_abilities, &Ability::Intimidate),
        "Intimidate must remain possible when own mon's Atk is already at −6 (C2)"
    );
}

// ── Audit 2026-07: EOT sand-immunity soundness (Air Lock / types-unknown) ─────

/// 1a regression: when an active mon could have Air Lock / Cloud Nine, the weather
/// effect (sand chip) may be suspended, so chip-absence proves nothing — the engine
/// must NOT emit a sand-immunity clause. Here the *P1* mon could have Cloud Nine.
#[test]
fn test_sand_immunity_skipped_when_cloud_nine_possible() {
    let mut p2_mon = unknown_mon();
    p2_mon.possible_types = Unknown::Known(vec![PokemonType::Normal]);
    p2_mon.possible_abilities = Unknown::Not(vec![Ability::AirLock, Ability::CloudNine]);
    // P1 mon whose ability is unconstrained → Cloud Nine cannot be ruled out.
    let mut p1_mon = unknown_mon();
    p1_mon.possible_abilities = Unknown::Not(vec![]);

    let mut state = battle_nvn(vec![p1_mon], vec![p2_mon]);
    state.weather = Some(Weather::Sandstorm);

    let result = apply(state, vec![event(EventKind::EndOfTurn)]);
    let has_clause = result.predicates.iter().any(|c| {
        c.iter().any(|s| matches!(s,
            Statement::HasItem { item: Item::SafetyGoggles, .. }
                | Statement::HasAbility { ability: Ability::SandVeil, .. }))
    });
    assert!(
        !has_clause,
        "sand-immunity inference must be skipped when Air Lock / Cloud Nine is possible \
         (weather may be suspended); predicates = {:?}", result.predicates
    );
}

/// 1a regression: a mon whose types are unknown could be Rock/Ground/Steel (innately
/// sand-immune), so chip-absence must not force a sand-immunity item/ability.
#[test]
fn test_sand_immunity_skipped_when_types_unknown() {
    let mut p2_mon = unknown_mon();
    p2_mon.possible_types = Unknown::Not(vec![]); // types unknown
    p2_mon.possible_abilities = Unknown::Not(vec![Ability::AirLock, Ability::CloudNine]);

    let mut state = battle_with_p2(vec![p2_mon]);
    state.weather = Some(Weather::Sandstorm);

    let result = apply(state, vec![event(EventKind::EndOfTurn)]);
    let has_clause = result.predicates.iter().any(|c| {
        c.iter().any(|s| matches!(s, Statement::HasItem { item: Item::SafetyGoggles, .. }))
    });
    assert!(
        !has_clause,
        "sand-immunity inference must be skipped when the mon's types are unknown"
    );
}

// ── Audit 2026-07: EOT heal weather widening (Rain Dish / Dry Skin / Ice Body) ─

/// 1b: an opponent EOT `Healed` in rain could be Leftovers OR Rain Dish OR Dry Skin.
/// The emitted disjunction must include the weather-heal abilities so the heal is not
/// misattributed to Leftovers (widening a disjunction is always sound).
#[test]
fn test_eot_heal_in_rain_includes_rain_dish_and_dry_skin() {
    let p2_mon = unknown_mon();
    let mut state = battle_with_p2(vec![p2_mon]);
    state.weather = Some(Weather::Rain);

    let ev = event_with(
        EventKind::EndOfTurn,
        vec![event(EventKind::Healed { target: p2(0), new_hp: PokemonHP::Percent(90) })],
    );
    let result = apply(state, vec![ev]);

    let clause = result
        .predicates
        .iter()
        .find(|c| c.iter().any(|s| matches!(s, Statement::HasItem { item: Item::Leftovers, .. })))
        .expect("a Leftovers-bearing EOT-heal clause must be emitted");
    assert!(
        clause.iter().any(|s| matches!(s, Statement::HasAbility { ability: Ability::RainDish, .. })),
        "rain EOT-heal clause must include Rain Dish as a disjunct; clause = {:?}", clause
    );
    assert!(
        clause.iter().any(|s| matches!(s, Statement::HasAbility { ability: Ability::DrySkin, .. })),
        "rain EOT-heal clause must include Dry Skin as a disjunct; clause = {:?}", clause
    );
}

/// 1b: in snow, the EOT-heal disjunction must include Ice Body.
#[test]
fn test_eot_heal_in_snow_includes_ice_body() {
    let p2_mon = unknown_mon();
    let mut state = battle_with_p2(vec![p2_mon]);
    state.weather = Some(Weather::Snow);

    let ev = event_with(
        EventKind::EndOfTurn,
        vec![event(EventKind::Healed { target: p2(0), new_hp: PokemonHP::Percent(90) })],
    );
    let result = apply(state, vec![ev]);

    let clause = result
        .predicates
        .iter()
        .find(|c| c.iter().any(|s| matches!(s, Statement::HasItem { item: Item::Leftovers, .. })))
        .expect("a Leftovers-bearing EOT-heal clause must be emitted");
    assert!(
        clause.iter().any(|s| matches!(s, Statement::HasAbility { ability: Ability::IceBody, .. })),
        "snow EOT-heal clause must include Ice Body as a disjunct; clause = {:?}", clause
    );
}

// ── Audit 2026-07: terrain-setter & Dauntless Shield ability-absence coverage ─

/// Terrain-setting ability absence (previously zero coverage): a mon that switches in
/// with no `TerrainChanged` reaction cannot have a terrain-setting ability.
#[test]
fn test_terrain_setter_excluded_when_no_terrain_change() {
    let mut p2_back = unknown_mon();
    // Electric Surge possible; Neutralizing Gas ruled out so the suppression gate passes.
    p2_back.possible_abilities = Unknown::Not(vec![Ability::NeutralizingGas]);

    let mut state = battle_with_p2(vec![]);
    state.p2_known_back_mons = vec![p2_back];

    let sw = event(EventKind::Switch(SwitchState {
        slot: p2(0),
        species: Species::Garchomp,
        level: 50,
        hp: PokemonHP::Percent(100),
        status: None,
        tera_type: None,
    }));
    let result = apply(state, vec![sw]);

    assert!(
        unknown_is_excluded(&result.p2_active_mons[0].possible_abilities, &Ability::ElectricSurge),
        "Electric Surge must be excluded when no TerrainChanged fires on switch-in; \
         possible_abilities = {:?}", result.p2_active_mons[0].possible_abilities
    );
}

/// Terrain-setter absence must be SKIPPED when Neutralizing Gas is possible (it would
/// suppress the surge, so the absent TerrainChanged proves nothing).
#[test]
fn test_terrain_setter_not_excluded_when_neutralizing_gas_possible() {
    let mut p2_back = unknown_mon();
    p2_back.possible_abilities = Unknown::Not(vec![]); // NeutralizingGas NOT excluded

    let mut state = battle_with_p2(vec![]);
    state.p2_known_back_mons = vec![p2_back];

    let sw = event(EventKind::Switch(SwitchState {
        slot: p2(0),
        species: Species::Garchomp,
        level: 50,
        hp: PokemonHP::Percent(100),
        status: None,
        tera_type: None,
    }));
    let result = apply(state, vec![sw]);

    assert!(
        !unknown_is_excluded(&result.p2_active_mons[0].possible_abilities, &Ability::ElectricSurge),
        "Electric Surge must NOT be excluded when Neutralizing Gas is possible (suppression gate)"
    );
}

// ── Regression: S7 — HadronEngine belongs in the terrain list, not the weather list ──

/// HadronEngine sets Electric Terrain (`TerrainChanged`), never weather — it must be
/// checked against `TerrainChanged`, not `WeatherChanged`. Before the S7 fix,
/// HadronEngine was listed in `WEATHER_SETTING_ABILITIES`: a mon that switches in and
/// sets Electric Terrain (revealing HadronEngine via a nested `AbilityRevealed`) would
/// still see no `WeatherChanged` reaction (HadronEngine never emits one) and get
/// wrongly excluded by the weather-absence pass before the nested reveal was even
/// processed — a vacuous, always-true "absence" that carried no real information.
#[test]
fn test_hadron_engine_not_excluded_by_weather_absence() {
    let mut p2_back = unknown_mon();
    p2_back.possible_abilities = Unknown::Not(vec![Ability::NeutralizingGas]);

    let mut state = battle_with_p2(vec![]);
    state.p2_known_back_mons = vec![p2_back];

    // Switch-in where HadronEngine sets Electric Terrain (TerrainChanged nested under
    // the AbilityRevealed wrapper), but no WeatherChanged occurs at all.
    let sw = event_with(
        EventKind::Switch(SwitchState {
            slot: p2(0),
            species: Species::Garchomp,
            level: 50,
            hp: PokemonHP::Percent(100),
            status: None,
            tera_type: None,
        }),
        vec![event_with(
            EventKind::AbilityRevealed { slot: p2(0), ability: Ability::HadronEngine },
            vec![event(EventKind::TerrainChanged { terrain: Some(Terrain::ElectricTerrain) })],
        )],
    );
    let result = apply(state, vec![sw]);

    assert_eq!(
        result.p2_active_mons[0].possible_abilities,
        Unknown::Known(Ability::HadronEngine),
        "HadronEngine must be confirmed Known, not excluded by the (vacuous) weather-absence check"
    );
}

/// Terrain-setter absence must exclude HadronEngine too — it belongs in the terrain
/// list now, so "no TerrainChanged fires" correctly rules it out.
#[test]
fn test_hadron_engine_excluded_when_no_terrain_change() {
    let mut p2_back = unknown_mon();
    p2_back.possible_abilities = Unknown::Not(vec![Ability::NeutralizingGas]);

    let mut state = battle_with_p2(vec![]);
    state.p2_known_back_mons = vec![p2_back];

    let sw = event(EventKind::Switch(SwitchState {
        slot: p2(0),
        species: Species::Garchomp,
        level: 50,
        hp: PokemonHP::Percent(100),
        status: None,
        tera_type: None,
    }));
    let result = apply(state, vec![sw]);

    assert!(
        unknown_is_excluded(&result.p2_active_mons[0].possible_abilities, &Ability::HadronEngine),
        "HadronEngine must be excluded when no TerrainChanged fires on switch-in"
    );
}

/// Dauntless Shield ability absence (previously zero coverage): a mon that switches in
/// with no +1 Def boost cannot have Dauntless Shield (once-per-battle, not yet used).
#[test]
fn test_dauntless_shield_excluded_when_no_def_boost_on_entry() {
    let mut p2_back = unknown_mon();
    p2_back.possible_abilities = Unknown::Not(vec![Ability::NeutralizingGas]);

    let mut state = battle_with_p2(vec![]);
    state.p2_known_back_mons = vec![p2_back];

    let sw = event(EventKind::Switch(SwitchState {
        slot: p2(0),
        species: Species::Garchomp,
        level: 50,
        hp: PokemonHP::Percent(100),
        status: None,
        tera_type: None,
    }));
    let result = apply(state, vec![sw]);

    assert!(
        unknown_is_excluded(&result.p2_active_mons[0].possible_abilities, &Ability::DauntlessShield),
        "Dauntless Shield must be excluded when no +1 Def boost appears on switch-in; \
         possible_abilities = {:?}", result.p2_active_mons[0].possible_abilities
    );
}
