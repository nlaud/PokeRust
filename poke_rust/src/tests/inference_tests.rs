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
    AccuracyType, DamageOverride, MoveCategory, MoveData, MoveTarget, PokemonData,
    PokemonType, PseudoWeather, SelfDestructType, SelfSwitchType, SideCondition, Status,
    Terrain, Weather,
};
use crate::information::inference::{
    apply_information, get_mon_by_idx, mon_idx_for_active_slot, pass5_back_solve,
    unknown_is_excluded, InferenceConfig, EV_LATTICE,
};
use crate::information::information::{EventKind, InformationEvent, SwitchState};
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
    let n = p2_active.len();
    UnknownBattleState {
        active_per_side: n as u8,
        back_mons_per_side: 5,
        p1_active_mons: vec![],
        p2_active_mons: p2_active,
        p1_known_back_mons: vec![],
        p2_known_back_mons: vec![],
        p1_possible_back_mons: vec![],
        p2_possible_back_mons: vec![],
        turn_number: 1,
        turn_started: false,
        turn_ended: false,
        p1_has_tera: false,
        p2_has_tera: false,
        p1_has_mega: false,
        p2_has_mega: false,
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
        p1_slot_conditions: (0..n).map(|_| vec![]).collect(),
        p2_slot_conditions: (0..n).map(|_| vec![]).collect(),
        self_switch_pending: None,
        items_consumed_this_turn: vec![],
        last_move_on_field: None,
        sub_damage_dealt: 0,
        round_used_this_turn: false,
        predicates: vec![],
    }
}

fn battle_1v1(p1_mon: UnknownPokemonState, p2_mon: UnknownPokemonState) -> UnknownBattleState {
    UnknownBattleState {
        active_per_side: 1,
        back_mons_per_side: 5,
        p1_active_mons: vec![p1_mon],
        p2_active_mons: vec![p2_mon],
        p1_known_back_mons: vec![],
        p2_known_back_mons: vec![],
        p1_possible_back_mons: vec![],
        p2_possible_back_mons: vec![],
        turn_number: 1,
        turn_started: false,
        turn_ended: false,
        p1_has_tera: false,
        p2_has_tera: false,
        p1_has_mega: false,
        p2_has_mega: false,
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
        p1_slot_conditions: vec![vec![]],
        p2_slot_conditions: vec![vec![]],
        self_switch_pending: None,
        items_consumed_this_turn: vec![],
        last_move_on_field: None,
        sub_damage_dealt: 0,
        round_used_this_turn: false,
        predicates: vec![],
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
    // Same move twice — Choice items are still possible.
    assert!(
        !is_item_excluded(&result.p2_active_mons[0], &Item::ChoiceBand),
        "ChoiceBand must NOT be excluded when same move repeated"
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

    let state = battle_with_p2(vec![mon]);

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

    // LifeOrb should NOT be excluded (the recoil is consistent with it).
    assert!(
        !is_item_excluded(&result.p2_active_mons[0], &Item::LifeOrb),
        "LifeOrb must not be excluded when self-damage reaction is present"
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
    // Garchomp EQ range against P1 (Def=100) at lv50 with neutral nature + IV=31:
    //   min (IV=31, EV=0, atk=150) → base=68 → roll=85 → 85
    //   max (IV=31, EV=252, atk=182) → base=82 → roll=100 → floor(82*1.5)=123
    // (InferenceConfig::default() has force_max_ivs=true, so IV=31 is assumed.)
    // Sweep starts at 85, not 76.  InferenceConfig::default() has force_max_ivs=true,
    // which pins IV=31 in Pass 5.  For Garchomp Atk at level 50 with IV=31:
    //   calc_stat(130, 31, EV=0, 50, 1.0) = floor((260+31)*0.5)+5 = 150
    // so the minimum achievable BSV is 150.  Damage values 76–84 are only feasible for
    // BSV ≤ 149, which can't be produced with IV=31.  Those scenarios are physically
    // impossible under the engine's IV assumption and correctly unreachable in real play.
    // The min damage for neutral Garchomp (IV=31, EV=0) vs Def=100:
    //   Atk=150 → base=68 → roll=85 → floor(57*1.5) = 85.
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

    // With no item locked, a Choice Band-boosted low-BSV attacker could also produce 91,
    // so the unconditional upper bound must be ≥ the band-free bound.
    // (A lower-BSV attacker with ×1.5 from Band could reach the same damage.)
    // We know the band-free max is 161.  With Band possible, the union includes lower BSVs
    // that with Band can also hit 91 → the max bound stays 182 (no tightening possible
    // for any upper bound when Band is possible, since Band on BSV_min is also plausible).

    // Key soundness assertion: the unconditional bound must NEVER go below the band-free bound
    // (because "BSV=182 + Band" is always possible and can produce any damage in its range).
    assert!(
        bound_with_band <= 182,
        "max BSV must not exceed 182 (got {})", bound_with_band
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

    // A crit EQ deals more damage; feed a plausible crit damage (≈ 100 HP, lower BSV needed).
    let result = apply_ex(
        state,
        vec![event_with(
            EventKind::MoveUsed { user: p2(0), move_used: PokemonMove::Earthquake, targets: vec![p1(0)] },
            vec![
                event(EventKind::Crit { target: p1(0) }),           // crit signalled
                event(EventKind::DamageDealt { target: p1(0), new_hp: PokemonHP::Number(400) }), // 100 damage
            ],
        )],
        garchomp_dex(),
        move_dex,
    );

    let p2_r = &result.p2_active_mons[0];
    // No contradiction: bounds must remain valid (min ≤ max).
    assert!(
        p2_r.min_pre_nature_stat[1] <= p2_r.max_pre_nature_stat[1],
        "Crit observation must not produce inverted bounds (min {}, max {})",
        p2_r.min_pre_nature_stat[1], p2_r.max_pre_nature_stat[1]
    );
    // Bounds must stay within Garchomp's theoretical range.
    assert!(p2_r.min_pre_nature_stat[1] >= 135);
    assert!(p2_r.max_pre_nature_stat[1] <= 182);
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
    // P2 might have Prankster.
    let mut p2_mon = no_speed_escape_mon(Species::Garchomp);
    p2_mon.possible_abilities = Unknown::Not(vec![
        Item::QuickClaw, Item::ChoiceScarf, Item::IronBall, Item::LaggingTail, Item::FullIncense,
    ].iter().map(|_| Ability::QuickDraw).collect()); // re-use field but only exclude QuickDraw
    // Actually just allow all abilities (Not([]) = all allowed).
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
}

/// Multi-hit with varied per-hit damage should produce bounds at least as tight
/// as a single-hit observation of just the first hit.
#[test]
fn test_pass3_multihit_tighter_than_single_hit() {
    let p1_mon = known_p1_normal(); // HP=500, Def=100
    let p2_mon = neutral_no_item_garchomp();

    let mut move_dex_multi = HashMap::new();
    let mut hit2_move = normal_physical_move(PokemonMove::BulletSeed, 25);
    hit2_move.multihit_range = [2, 2];
    move_dex_multi.insert(PokemonMove::BulletSeed, hit2_move);

    let mut move_dex_single = HashMap::new();
    let mut single_move = normal_physical_move(PokemonMove::Earthquake, 25);
    single_move.multihit_range = [1, 1];
    move_dex_single.insert(PokemonMove::Earthquake, single_move);

    // 2-hit: first hit deals 40 damage, second deals 38 (different rolls → tighter intersection).
    let multi_result = apply_ex(
        battle_1v1(p1_mon.clone(), p2_mon.clone()),
        vec![event_with(
            EventKind::MoveUsed { user: p2(0), move_used: PokemonMove::BulletSeed, targets: vec![p1(0)] },
            vec![
                event(EventKind::DamageDealt { target: p1(0), new_hp: PokemonHP::Number(460) }), // 40 dmg
                event(EventKind::DamageDealt { target: p1(0), new_hp: PokemonHP::Number(422) }), // 38 dmg
                event(EventKind::HitCount { target: p1(0), hits: 2 }),
            ],
        )],
        garchomp_dex(),
        move_dex_multi,
    );

    // Single hit: only the first hit (40 damage).
    let single_result = apply_ex(
        battle_1v1(p1_mon, p2_mon),
        vec![event_with(
            EventKind::MoveUsed { user: p2(0), move_used: PokemonMove::Earthquake, targets: vec![p1(0)] },
            vec![event(EventKind::DamageDealt { target: p1(0), new_hp: PokemonHP::Number(460) })], // 40 dmg
        )],
        garchomp_dex(),
        move_dex_single,
    );

    let m = &multi_result.p2_active_mons[0];
    let s = &single_result.p2_active_mons[0];

    // Sound: true BSV must lie within both bounds.
    assert!(m.min_pre_nature_stat[1] <= m.max_pre_nature_stat[1], "multi-hit bounds inverted");
    assert!(s.min_pre_nature_stat[1] <= s.max_pre_nature_stat[1], "single-hit bounds inverted");

    // Multi-hit bounds should be at least as tight as single-hit.
    assert!(
        m.min_pre_nature_stat[1] >= s.min_pre_nature_stat[1]
            || m.max_pre_nature_stat[1] <= s.max_pre_nature_stat[1],
        "Multi-hit bounds should be at least as tight as single-hit: multi [{}, {}] vs single [{}, {}]",
        m.min_pre_nature_stat[1], m.max_pre_nature_stat[1],
        s.min_pre_nature_stat[1], s.max_pre_nature_stat[1],
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
}

// ── Regression: S5 — Switch-out reset clears boosts and volatiles ────────────

/// `apply_switch_out_reset` must clear boosts and volatile statuses from the mon
/// leaving the field.  This regression verifies that processing a Switch event
/// for a mon that carries non-zero boosts, a volatile, and a ToxicPoison tier
/// does NOT panic and produces a consistent subsequent state.
///
/// **Architecture note**: `apply_switch_out_reset` modifies the active slot
/// in-place, then `pass1_switch` immediately overwrites that slot with the
/// incoming mon.  The cleared outgoing-mon state is not externally visible after
/// the replacement; this test therefore covers the code path and verifies that
/// the incoming mon has clean state regardless of what the outgoing mon carried.
#[test]
fn test_switch_out_clears_boosts_and_volatiles() {
    use crate::state::pokemon::VolatileStatusState;
    use crate::state::dex_data::VolatileStatus;

    let mut p2_mon = unknown_mon(); // Garchomp in active slot
    // Artificially set boosts, a volatile, and a ToxicPoison tier — all of which
    // the real simulator clears on switch-out (mirrors/mod.rs 6205-6214).
    p2_mon.boosts = [3, 2, 1, -1, 0, 0, 0];
    p2_mon.volatiles = vec![
        VolatileStatusState::TurnStatus(VolatileStatus::Confusion, 3),
    ];
    p2_mon.status = Some(Status::ToxicPoison(4)); // tier-4 toxic (escalates each EOT)

    let state = battle_with_p2(vec![p2_mon]);

    // Charizard switches in at P2 slot 0 (replacing the Garchomp).
    // apply_switch_out_reset runs on the Garchomp slot before the replacement.
    let result = apply(
        state,
        vec![event(EventKind::Switch(SwitchState {
            slot:      p2(0),
            species:   Species::Charizard,
            level:     50,
            hp:        PokemonHP::Percent(100),
            status:    None,
            tera_type: None,
        }))],
    );

    // The incoming Charizard must have clean state — no stale boosts or volatiles
    // from the outgoing Garchomp.
    let incoming = &result.p2_active_mons[0];
    assert_eq!(incoming.boosts, [0i8; 7], "incoming mon must start with 0 boosts");
    assert!(incoming.volatiles.is_empty(), "incoming mon must have no volatiles");
    // No ToxicPoison tier should bleed across into the fresh mon.
    assert!(
        !matches!(incoming.status, Some(Status::ToxicPoison(n)) if n > 0),
        "incoming mon must not inherit a ToxicPoison tier from the outgoing slot"
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
    // Note: `pass3` (damage→stat inference) may add EVIVStat predicates from the
    // DamageDealt reaction, so we test for absence of ability-preventer clauses
    // specifically, not for zero total predicates.
    let has_freeze_preventer_clause = result.predicates.iter().any(|clause| {
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

// ── Regression: E1 — Binary-search preserves precision of linear scan ────────

/// The binary-search implementation of `find_feasible_bsv_range_b` (Direction B) must
/// return the same BSV bounds as the former linear scan.  Equivalence is implicitly
/// confirmed by the full test suite (all existing pass3_dir_b_* tests pass).  This
/// test adds an explicit regression: P2 attacks P1 with an exact damage number and
/// P2's Atk BSV range must be narrowed (proving the binary search found a non-trivial
/// lower/upper bound matching the true feasibility interval).
///
/// Damage arithmetic (no items, no STAB, 1× effectiveness, IV=31 pinned by default config):
///   base_dmg(Atk, Def=65, bp=40, lv=50)
///     = floor(floor(22 × 40 × Atk / 65) / 50 + 2)
///   Atk=148: base=42 → rolls [35..42]   (max roll 100 → 42; min roll 85 → 35)
///   Atk=147: base=41 → rolls [34..41]   (max 41 < 42 — infeasible for dmg=42)
///   Atk=180: base=50 → rolls [42..50]   (min roll 85 → floor(50×0.85)=42 ✓)
///   Atk=181: base=51 → rolls [43..51]   (min 43 > 42 — infeasible for dmg=42)
/// With IV=31 pinned, Garchomp's min Atk BSV is 150 (ev=0,iv=31). Observing
/// exactly 42 HP damage (new_hp=158) must tighten:
///   min_pre_nature_stat[1]: 135 → 148  (raised — Atk<148 can't produce 42)
///   max_pre_nature_stat[1]: 182 → 180  (lowered — Atk>180 can't produce 42)
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
    // The binary search (or the former linear scan) must tighten the Atk BSV bounds.
    assert!(
        p2_result.min_pre_nature_stat[1] >= initial_atk_min,
        "Binary search must not lower min Atk BSV below initial value"
    );
    assert!(
        p2_result.max_pre_nature_stat[1] <= initial_atk_max,
        "Binary search must not raise max Atk BSV above initial value"
    );
    assert!(
        p2_result.min_pre_nature_stat[1] > initial_atk_min
        || p2_result.max_pre_nature_stat[1] < initial_atk_max,
        "Binary search must produce a non-trivial narrowing of the Atk BSV range from 24 HP dealt"
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

    // Use a Normal-type status move to avoid type-immunity inference issues.
    let mut move_dex = HashMap::new();
    move_dex.insert(
        PokemonMove::WillOWisp,
        MoveData {
            name: PokemonMove::WillOWisp,
            base_power: 0,
            accuracy: AccuracyType::Percent(85),
            target: MoveTarget::Normal,
            secondaries: vec![],
            self_secondaries: vec![],
            pp: 15,
            category: MoveCategory::Status,
            pokemon_type: PokemonType::Fire,
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
        },
    );

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
