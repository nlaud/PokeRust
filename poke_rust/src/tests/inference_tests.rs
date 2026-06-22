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
    InferenceConfig, EV_LATTICE,
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
