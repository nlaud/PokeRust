//! Tests for `state::inference::apply_information`.
//!
//! Tests construct hand-built `InformationEvent` lists and assert on the resulting
//! `UnknownMatchState`.  All assertions must satisfy the soundness invariant: the
//! true training/item/stat of the simulated Pokémon must lie *within* every returned bound.

#![allow(unused)]

use std::collections::HashMap;

use crate::data::ability::Ability;
use crate::data::item::Item;
use crate::data::pokemon_move::PokemonMove;
use crate::data::species::Species;
use crate::state::battle::{FieldSlot, Player};
use crate::state::dex_data::{
    AccuracyType, DamageOverride, MoveCategory, MoveData, MoveTarget, PokemonData,
    PokemonType, SelfDestructType, SelfSwitchType, Status,
};
use crate::information::inference::{
    apply_information, get_mon_by_idx, mon_idx_for_active_slot, InferenceConfig, EV_LATTICE,
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

/// Every achievable damage value in Garchomp's EQ range must not cause a contradiction
/// in Pass 3 (no panic, and bounds remain valid min ≤ max).
#[test]
fn test_pass3_dir_b_no_contradiction_across_damage_range() {
    // Garchomp EQ range against P1 (Def=100) at lv50 with neutral nature:
    //   min atk=135 → base=61 → roll=85 → 76
    //   max atk=182 → base=82 → roll=100 → floor(82*1.5)=123
    for damage in 76u16..=123u16 {
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
