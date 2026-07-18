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
        p1_fainted_mons: vec![],
        p2_fainted_mons: vec![],
        p1_unresolved_zoroark_count: 0,
        p2_unresolved_zoroark_count: 0,
        p1_roster_templates: vec![],
        p2_roster_templates: vec![],
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

/// Like [`apply_ex`], but against real dex/move-dex references (mirrors
/// `roundtrip_soundness::apply_roundtrip`) — `apply_ex` demands OWNED dex maps,
/// which doesn't work for real-dex tests since `PokemonData`/`MoveData` aren't
/// `Clone`.
fn apply_real_dex(
    state: UnknownBattleState,
    events: Vec<InformationEvent>,
    dex: &HashMap<Species, PokemonData>,
    move_dex: &HashMap<PokemonMove, MoveData>,
) -> UnknownBattleState {
    let result = apply_information(
        UnknownMatchState::Battle(state),
        &events,
        false,
        dex,
        move_dex,
        &HashMap::new(), // ability_dex — not needed for these tests
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

/// S44: a Fire-type/`thawsTarget` move thawing a Frozen target used to be a silent
/// `mon.status = None` in `simulator::apply_single_hit_branch` with no matching
/// `StatusCured` event — the observer's belief kept tracking Frozen, so a later
/// status-inflicting effect surfaced as "StatusInflicted X but already has Frozen",
/// an inference contradiction on ordinary, legal play. The actual fix is on the
/// simulator side (see `fire_move_unfreeze_emits_status_cured_event` in
/// `simulator_tests.rs`, which asserts the event is now emitted at all); this
/// companion test locks in the OTHER half of the contract — that once the cure is
/// correctly emitted, the belief round-trips it cleanly with no panic — mirroring
/// `test_status_inflicted_then_cured` above for the Frozen-then-Burn transition.
#[test]
fn test_s44_status_cured_then_new_status_does_not_panic() {
    let state = battle_with_p2(vec![unknown_mon()]);
    let result = apply(
        state,
        vec![
            event(EventKind::StatusInflicted { target: p2(0), status: Status::Frozen(0) }),
            event(EventKind::StatusCured { target: p2(0), status: Status::Frozen(0) }),
            event(EventKind::StatusInflicted { target: p2(0), status: Status::Burn }),
        ],
    );
    assert_eq!(result.p2_active_mons[0].status, Some(Status::Burn));
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
        vec![event(EventKind::DamageDealt { max_hp: 0,
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
                    event(EventKind::DamageDealt { max_hp: 0, target: p1(0), new_hp: PokemonHP::Number(200) }),
                    event(EventKind::DamageDealt { max_hp: 0, target: p2(0), new_hp: PokemonHP::Percent(90) }),
                ],
            ),
            // EOT chip on P2 with no enclosing MoveUsed (e.g. Sandstorm).
            event_with(
                EventKind::EndOfTurn,
                vec![event(EventKind::DamageDealt { max_hp: 0, target: p2(0), new_hp: PokemonHP::Percent(80) })],
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
        vec![event(EventKind::Switch(SwitchState { disguise_species: None, max_hp: 0,
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

/// Regression: a fainted-then-replaced opponent mon must land in `p2_fainted_mons`
/// (with its accumulated knowledge intact — here, a revealed move) rather than
/// being silently discarded. Before the fix, `bench_outgoing_mon` filtered out
/// `fainted` outgoing mons entirely (`filter(|m| !m.fainted)`) instead of routing
/// them anywhere, so the belief retained no record the mon ever existed once its
/// slot was overwritten by the replacement — it showed up in neither "back" nor
/// "fainted" in the UI.
#[test]
fn test_fainted_opponent_routed_to_fainted_bucket_on_replacement() {
    let mut p2_mon = unknown_mon_species(Species::Charizard);
    // Simulate knowledge accumulated earlier in the battle (a revealed move) that
    // must survive the faint + replacement, not be lost with the discarded mon.
    p2_mon.known_moves[0] = Some(PokemonMove::Flamethrower);
    let state = battle_1v1(unknown_mon_species(Species::Garchomp), p2_mon);

    let mut move_dex = HashMap::new();
    move_dex.insert(PokemonMove::Tackle, normal_physical_move(PokemonMove::Tackle, 40));

    let result = apply_ex(
        state,
        vec![
            // P1 KOs P2's Charizard.
            event_with(
                EventKind::MoveUsed { user: p1(0), move_used: PokemonMove::Tackle, targets: vec![p2(0)] },
                vec![
                    event(EventKind::DamageDealt { max_hp: 0, target: p2(0), new_hp: PokemonHP::Percent(0) }),
                    event(EventKind::Faint { slot: p2(0) }),
                ],
            ),
            // P2 sends out a replacement into the now-empty slot.
            event(EventKind::Switch(SwitchState {
                disguise_species: None,
                max_hp: 0,
                slot: p2(0),
                species: Species::Blastoise,
                level: 50,
                hp: PokemonHP::Percent(100),
                status: None,
                tera_type: None,
            })),
        ],
        HashMap::new(),
        move_dex,
    );

    assert_eq!(result.p2_fainted_mons.len(), 1, "fainted Charizard should be retained in the fainted bucket");
    let fainted = &result.p2_fainted_mons[0];
    assert!(
        matches!(&fainted.possible_species, Unknown::Known(s) if *s == Species::Charizard),
        "fainted bucket should hold the fainted mon's species, got {:?}", fainted.possible_species
    );
    assert!(fainted.fainted, "fainted-bucket entry must still be marked fainted");
    assert_eq!(
        fainted.known_moves[0],
        Some(PokemonMove::Flamethrower),
        "knowledge revealed before the faint must survive into the fainted bucket"
    );

    assert!(
        matches!(&result.p2_active_mons[0].possible_species, Unknown::Known(s) if *s == Species::Blastoise),
        "replacement should occupy the active slot"
    );
    assert!(result.p2_known_back_mons.is_empty(), "fainted mon must not also appear in known_back");
    assert!(result.p2_possible_back_mons.is_empty(), "fainted mon must not also appear in possible_back");
}

// ── Pass 2: Life Orb exclusion ────────────────────────────────────────────────

#[test]
fn test_no_recoil_excludes_life_orb() {
    // Rule out MagicGuard and SheerForce; Earthquake has no secondary → SheerForce irrelevant.
    // Klutz is also ruled out (S21): a Klutz attacker's Life Orb never chips, so the
    // hard exclusion requires Klutz impossible as well.
    let mut mon = unknown_mon();
    mon.possible_abilities =
        Unknown::Not(vec![Ability::MagicGuard, Ability::SheerForce, Ability::Klutz]);

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
            vec![event(EventKind::DamageDealt { max_hp: 0,
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
                event(EventKind::DamageDealt { max_hp: 0,
                    target: p1(0),
                    new_hp: PokemonHP::Number(200),
                }),
                event(EventKind::DamageDealt { max_hp: 0,
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
                event(EventKind::DamageDealt { max_hp: 0,
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

// ── Regression: S18 — slot re-binding must not inherit the old occupant's clauses ──

/// A unit SpeedComparison recorded for the Snorlax in P2 slot 0 must be dropped when
/// Snorlax switches out: the slot index now denotes the incoming Aggron, and before
/// the S18 fix the persisted comparison re-bound to it — raising the fresh switch-in's
/// min Spe to the previous occupant's evidence (excluding every true slower-Aggron
/// world, and panicking when the forced min exceeded the species' max).
#[test]
fn test_s18_speed_comparison_purged_on_switch() {
    let mut p1_mon = unknown_mon_species(Species::Garchomp);
    p1_mon.min_stats[5] = 150;
    p1_mon.max_stats[5] = 150;

    let p2_mon = unknown_mon_species(Species::Snorlax);
    let mut state = battle_1v1(p1_mon, p2_mon);
    // Evidence from a previous turn: P2 slot 0 (Snorlax) outsped our 150-Spe mon.
    state.predicates.push(vec![Statement::SpeedComparison {
        fast_idx: 1,
        slow_idx: 0,
        fast_mult: 1,
        slow_mult: 1,
    }]);

    let fresh_min = unknown_mon_species(Species::Aggron).min_stats[5];
    let result = apply(
        state,
        vec![event(EventKind::Switch(SwitchState { disguise_species: None, max_hp: 0,
            slot: p2(0),
            species: Species::Aggron,
            level: 50,
            hp: PokemonHP::Percent(100),
            status: None,
            tera_type: None,
        }))],
    );

    let incoming = &result.p2_active_mons[0];
    assert!(matches!(&incoming.possible_species, Unknown::Known(s) if *s == Species::Aggron));
    assert_eq!(
        incoming.min_stats[5], fresh_min,
        "the previous occupant's SpeedComparison must not constrain the switch-in"
    );
    // The Snorlax-scoped clause must be gone from the store.
    let stale_clause = result.predicates.iter().any(|clause| {
        clause.iter().any(|s| matches!(s, Statement::SpeedComparison { fast_idx: 1, .. }))
    });
    assert!(!stale_clause, "mon-scoped clauses must be purged when the slot occupant changes");
}

/// Item-disjunction analogue: `[HasItem(BrightPowder) ∨ HasItem(LaxIncense)]` recorded
/// for the outgoing occupant must not survive to the switch-in — before the fix, a
/// later `ItemRevealed{Leftovers}` on the NEW mon falsified both literals and
/// panicked with an unsatisfiable clause, although both physical mons' items were
/// perfectly consistent.
#[test]
fn test_s18_item_clause_purged_on_switch() {
    let p1_mon = unknown_mon_species(Species::Garchomp);
    let p2_mon = unknown_mon_species(Species::Snorlax);
    let mut state = battle_1v1(p1_mon, p2_mon);
    state.predicates.push(vec![
        Statement::HasItem { mon_idx: 1, item: Item::BrightPowder },
        Statement::HasItem { mon_idx: 1, item: Item::LaxIncense },
    ]);

    // Snorlax leaves; Aggron enters and reveals Leftovers.
    let result = apply(
        state,
        vec![
            event(EventKind::Switch(SwitchState { disguise_species: None, max_hp: 0,
                slot: p2(0),
                species: Species::Aggron,
                level: 50,
                hp: PokemonHP::Percent(100),
                status: None,
                tera_type: None,
            })),
            event(EventKind::ItemRevealed { slot: p2(0), item: Item::Leftovers }),
        ],
    );
    assert!(matches!(&result.p2_active_mons[0].item, Unknown::Known(i) if *i == Item::Leftovers));
}

// ── Regression: S19 — HasItem clauses are resolved when the held item changes ───

/// A miss-explanation clause `[HasItem(BrightPowder) ∨ HasItem(LaxIncense)]` is
/// recorded, then Knock Off removes the mon's Bright Powder — a world fully
/// consistent with the clause (it held Bright Powder at observation time). Before
/// the S19 fix, BCP evaluated the stale clause against the now-`Known(None)` item,
/// falsified both literals, and panicked with an unsatisfiable clause.
#[test]
fn test_s19_knock_off_resolves_stale_item_clause() {
    let p1_mon = unknown_mon_species(Species::Garchomp);
    let p2_mon = unknown_mon_species(Species::Snorlax);
    let mut state = battle_1v1(p1_mon, p2_mon);
    state.predicates.push(vec![
        Statement::HasItem { mon_idx: 1, item: Item::BrightPowder },
        Statement::HasItem { mon_idx: 1, item: Item::LaxIncense },
    ]);

    let result = apply(
        state,
        vec![event(EventKind::ItemLost {
            slot: p2(0),
            item: Item::BrightPowder,
            consumed: false,
        })],
    );
    // The clause was historically satisfied by the knocked-off item → dropped.
    assert!(
        result.predicates.is_empty(),
        "historically-satisfied item clause must be dropped, got {:?}",
        result.predicates
    );
    assert!(matches!(&result.p2_active_mons[0].item, Unknown::Known(i) if *i == Item::None));
}

/// Companion precision check: consuming a berry proves the setter never held the
/// weather rock, so the `[HasItem(DampRock) ∨ WeatherTurns{3}]` pair must collapse
/// to the base-duration branch (the rock literal is pruned as historically false and
/// the surviving unit `WeatherTurns{3}` is forced into the timer).
#[test]
fn test_s19_berry_consumption_collapses_weather_timer_pair() {
    let p1_mon = unknown_mon_species(Species::Garchomp);
    let p2_mon = unknown_mon_species(Species::Pelipper);
    let mut state = battle_1v1(p1_mon, p2_mon);
    state.weather = Some(Weather::Rain);
    state.weather_turns = Some(Unknown::Possibly(vec![3, 6]));
    state.weather_setter_mon_idx = Some(1);
    state.predicates.push(vec![
        Statement::HasItem { mon_idx: 1, item: Item::DampRock },
        Statement::WeatherTurns { turns: 3 },
    ]);
    state.predicates.push(vec![
        Statement::Not(Box::new(Statement::HasItem { mon_idx: 1, item: Item::DampRock })),
        Statement::WeatherTurns { turns: 6 },
    ]);

    let result = apply(
        state,
        vec![event(EventKind::ItemLost {
            slot: p2(0),
            item: Item::SitrusBerry,
            consumed: true,
        })],
    );
    // Held item was Sitrus, not Damp Rock → the 5-turn (base) branch is proven.
    assert_eq!(
        result.weather_turns,
        Some(Unknown::Known(3)),
        "consuming a non-rock item must collapse the timer to the base-duration branch"
    );
}

// ── Regression: S26 — Transform overlay and switch-out revert ───────────────────

/// An opponent Ditto uses Transform (a `Transformed` reaction nested under the
/// `MoveUsed`) to copy our Garchomp, gaining its moves; then switches out. The copied
/// move (Earthquake) must NOT persist on the benched Ditto after the revert — before
/// S26 there was no Transform handling at all, so a copied move stayed burned into
/// the mon's own `known_moves` permanently, corrupting later Choice-lock / learnset
/// reasoning. The revert also restores the pre-transform species (Ditto).
#[test]
fn test_s26_transform_reverts_moves_on_switch_out() {
    // Our P1 Garchomp with a known move (the copy source's move set).
    let mut p1_mon = unknown_mon_species(Species::Garchomp);
    p1_mon.known_moves[0] = Some(PokemonMove::Earthquake);
    // Opponent Ditto, currently active.
    let p2_mon = unknown_mon_species(Species::Ditto);
    let state = battle_1v1(p1_mon, p2_mon);

    let mut move_dex = HashMap::new();
    move_dex.insert(PokemonMove::Transform, poke_status_move(PokemonMove::Transform));
    move_dex.insert(PokemonMove::Earthquake, normal_physical_move(PokemonMove::Earthquake, 100));

    let result = apply_ex(
        state,
        vec![
            // Ditto Transforms into our Garchomp: copies its identity + moves.
            event_with(
                EventKind::MoveUsed { user: p2(0), move_used: PokemonMove::Transform, targets: vec![p1(0)] },
                vec![event(EventKind::Transformed {
                    slot: p2(0),
                    into_slot: p1(0),
                    into_species: Species::Garchomp,
                })],
            ),
            // …then switches out for a fresh Snorlax.
            event(EventKind::Switch(SwitchState { disguise_species: None, max_hp: 0,
                slot: p2(0), species: Species::Snorlax, level: 50,
                hp: PokemonHP::Percent(100), status: None, tera_type: None,
            })),
        ],
        garchomp_dex(),
        move_dex,
    );

    // The benched mon must have reverted to Ditto (species restored), with no copied
    // Earthquake and no lingering pre_transform snapshot.
    let ditto = result
        .p2_known_back_mons
        .iter()
        .find(|m| matches!(&m.possible_species, Unknown::Known(Species::Ditto)))
        .expect("the transformed Ditto must revert to Ditto on switch-out");
    assert!(
        !ditto.known_moves.contains(&Some(PokemonMove::Earthquake)),
        "a move copied while transformed must not persist after the revert"
    );
    assert!(ditto.pre_transform.is_none(), "pre_transform must clear on revert");
}

/// While transformed, the mon's copied stats must NOT be back-solved against its own
/// (Ditto) species base — Pass 5 is skipped for a transformed mon. This pins that the
/// overlay + gate run without a contradiction panic even though Garchomp's copied
/// stats are impossible for Ditto's base stats + EV lattice.
#[test]
fn test_s26_transform_skips_pass5_backsolve() {
    let mut p1_mon = unknown_mon_species(Species::Garchomp);
    p1_mon.known_moves[0] = Some(PokemonMove::Earthquake);
    let p2_mon = unknown_mon_species(Species::Ditto);
    let state = battle_1v1(p1_mon, p2_mon);

    let mut move_dex = HashMap::new();
    move_dex.insert(PokemonMove::Transform, poke_status_move(PokemonMove::Transform));

    // Must not panic (pass5 skip) and must display Garchomp with a saved snapshot.
    let result = apply_ex(
        state,
        vec![event_with(
            EventKind::MoveUsed { user: p2(0), move_used: PokemonMove::Transform, targets: vec![p1(0)] },
            vec![event(EventKind::Transformed {
                slot: p2(0),
                into_slot: p1(0),
                into_species: Species::Garchomp,
            })],
        )],
        garchomp_dex(),
        move_dex,
    );

    let t = &result.p2_active_mons[0];
    assert!(matches!(&t.possible_species, Unknown::Known(Species::Garchomp)));
    assert!(t.pre_transform.is_some(), "pre_transform snapshot must be saved");
}

// ── Regression: S25 — pinch abilities must be live for a low-HP attacker ────────

/// A Garchomp at 25% display HP with Blaze still possible (and item Known(None), so
/// no Choice Band alias can cover for it) uses a 40 BP Fire move. True world: min
/// Atk BSV 135 + Blaze ×1.5 (BP 60 → base term 37), observed damage 31 (roll 0.85).
///
/// Before the S25 fix, materialize mapped any non-100% display HP to 0.5×max, so the
/// ≤1/3 pinch gate never fired in the oracle: every enumerated combo degenerated to
/// the neutral model (BP 40), for which damage 31 is only feasible at BSV ∈
/// ≈[165, 182] — raising min_pre_nature_stat[1] above the true 135.
#[test]
fn test_s25_blaze_pinch_keeps_true_atk_feasible() {
    let mut p1_mon = known_p1_normal(); // HP=500, Def=100, Normal type (Fire → ×1)
    let mut p2_mon = neutral_no_item_garchomp();
    // Blaze remains possible; ability not Known so the neutral run uses Ability::None.
    p2_mon.possible_abilities = Unknown::Possibly(vec![Ability::Blaze, Ability::SandVeil]);
    p2_mon.hp = PokemonHP::Percent(25); // certainly at ≤1/3 HP

    let mut fire_move = normal_physical_move(PokemonMove::FirePunch, 40);
    fire_move.pokemon_type = PokemonType::Fire; // no STAB for Dragon/Ground Garchomp
    let mut move_dex = HashMap::new();
    move_dex.insert(PokemonMove::FirePunch, fire_move);

    let state = battle_1v1(p1_mon, p2_mon);
    let result = apply_ex(
        state,
        vec![event_with(
            EventKind::MoveUsed { user: p2(0), move_used: PokemonMove::FirePunch, targets: vec![p1(0)] },
            vec![event(EventKind::DamageDealt { max_hp: 0, target: p1(0), new_hp: PokemonHP::Number(469) })],
        )],
        garchomp_dex(),
        move_dex,
    );

    let p2_r = &result.p2_active_mons[0];
    assert!(
        p2_r.min_pre_nature_stat[1] <= 135,
        "true Atk BSV 135 (+ active Blaze) must remain feasible; got min {} — the \
         0.5×max HP sentinel disables the pinch gate and forces the neutral model",
        p2_r.min_pre_nature_stat[1]
    );
}

// ── Regression: S24 — Pass 3 must run against the PRE-move state ────────────────

/// A Power-Up-Punch-style move raises the user's Atk AFTER the hit; Pass 1 applies
/// that `BoostChanged` before Pass 3 runs, so the oracle used to model the observed
/// (unboosted) damage at +1 Atk.
///
/// Hand-derived (Garchomp, neutral, 40 BP Normal vs Def=100, formula
/// `floor(floor(22·40·A/100)/50)+2`, rolls 0.85–1.00): true Atk BSV = 182 → base
/// term 34 → non-crit damage 28–34; observed 34 (roll 1.0). With the post-move +1
/// stage baked in, damage 34 needs boosted A ∈ [182, 221] → BSV ∈ ≈[135, 147] —
/// capping the bound at 147 and excluding the true 182. The S24 snapshot restores
/// the pre-move (stage 0) view, where BSV = 182 is exactly feasible.
#[test]
fn test_s24_self_boost_secondary_keeps_true_atk_feasible() {
    let p1_mon = known_p1_normal(); // HP=500, Def=100
    let p2_mon = neutral_no_item_garchomp();

    let mut move_dex = HashMap::new();
    move_dex.insert(PokemonMove::Tackle, normal_physical_move(PokemonMove::Tackle, 40));

    let state = battle_1v1(p1_mon, p2_mon);
    let result = apply_ex(
        state,
        vec![event_with(
            EventKind::MoveUsed { user: p2(0), move_used: PokemonMove::Tackle, targets: vec![p1(0)] },
            vec![
                // The hit itself: 34 damage (500 → 466), dealt at stage 0.
                event(EventKind::DamageDealt { max_hp: 0, target: p1(0), new_hp: PokemonHP::Number(466) }),
                // The self-boost lands AFTER the hit.
                event(EventKind::BoostChanged { target: p2(0), boost_idx: 0, stages: 1 }),
            ],
        )],
        garchomp_dex(),
        move_dex,
    );

    let p2_r = &result.p2_active_mons[0];
    // The live boost must still be recorded (Pass 1 is unchanged) …
    assert_eq!(p2_r.boosts[0], 1, "the +1 Atk stage must be tracked on the live mon");
    // … but Pass 3 must have run against the pre-boost snapshot.
    assert!(
        p2_r.min_pre_nature_stat[1] <= 182 && 182 <= p2_r.max_pre_nature_stat[1],
        "true Atk BSV 182 must stay within [{}, {}] — modeling the observed damage \
         at the post-move +1 stage excludes it",
        p2_r.min_pre_nature_stat[1],
        p2_r.max_pre_nature_stat[1]
    );
}

// ── Regression: S27 — Metronome streak resets on zero-effective-damage moves ────

/// The sim resets `consecutive_move_count` to 0 and nulls `last_used_move` when a
/// damaging move deals no effective damage (miss / immune / fully blocked). Before
/// the S27 fix, inference incremented the streak on every `MoveUsed` regardless of a
/// `Missed` reaction, so the fog streak drifted above the sim's — and the drifted
/// value is materialized straight into Pass 3 oracle calls (Metronome ×(1+0.2n)).
#[test]
fn test_s27_streak_resets_on_missed_move() {
    let p1_mon = unknown_mon_species(Species::Garchomp);
    let p2_mon = unknown_mon_species(Species::Snorlax);
    let state = battle_1v1(p1_mon, p2_mon);

    let mut move_dex = HashMap::new();
    move_dex.insert(PokemonMove::Tackle, normal_physical_move(PokemonMove::Tackle, 40));

    let result = apply_ex(
        state,
        vec![
            // First use connects: streak 0, last_used = Tackle.
            event_with(
                EventKind::MoveUsed { user: p2(0), move_used: PokemonMove::Tackle, targets: vec![p1(0)] },
                vec![event(EventKind::DamageDealt { max_hp: 0, target: p1(0), new_hp: PokemonHP::Percent(90) })],
            ),
            // Second use misses: the sim resets the streak and nulls last_used_move.
            event_with(
                EventKind::MoveUsed { user: p2(0), move_used: PokemonMove::Tackle, targets: vec![p1(0)] },
                vec![event(EventKind::Missed { target: p1(0) })],
            ),
        ],
        HashMap::new(),
        move_dex,
    );

    let mon = &result.p2_active_mons[0];
    assert_eq!(
        mon.consecutive_move_count, 0,
        "a missed damaging move must reset the streak (sim: total_effective_dmg == 0)"
    );
    assert_eq!(
        mon.last_used_move, None,
        "a missed damaging move must null last_used_move (sim parity)"
    );
}

// ── Regression: S22 — Direction A damage band must cover both display roundings ──

/// Exhaustive cross-validation of the Pass 3 percent→damage band against the real
/// display convention: for every (pre_raw, post_raw) HP pair of a large-HP mon, the
/// true damage must lie inside the band derived from the two DISPLAY percents.
///
/// Before the S22 fix the band was `[(δ−0.5)%, (δ+0.5)%]` of max HP — treating the
/// delta as a single rounding although pre and post each round independently. For a
/// 362-HP mon that band excluded achievable damages near the bucket edges, silently
/// raising the defensive-BSV floor above the true value.
#[test]
fn test_s22_percent_damage_band_covers_all_roundings() {
    use crate::information::inference::percent_bucket;
    use crate::simulator::helpers::hp_to_percent;

    for &max_hp in &[75u16, 155, 207, 362] {
        for pre_raw in 1..=max_hp {
            // Sample post_raw across the range (full cross product is 130k+ pairs
            // per max_hp; stride keeps the test fast while covering all bucket edges).
            for post_raw in (0..pre_raw).step_by(1) {
                let pre_pct = hp_to_percent(pre_raw, max_hp);
                let post_pct = hp_to_percent(post_raw, max_hp);
                if post_pct >= pre_pct {
                    continue; // display shows no drop → Pass 3 never fires
                }
                let true_damage = pre_raw - post_raw;

                let (pre_lo, pre_hi) = percent_bucket(pre_pct, max_hp)
                    .expect("observed display percent must have a bucket");
                let (post_lo, post_hi) = percent_bucket(post_pct, max_hp)
                    .expect("observed display percent must have a bucket");
                let d_lo = pre_lo.saturating_sub(post_hi).max(1);
                let d_hi = pre_hi.saturating_sub(post_lo);

                assert!(
                    d_lo <= true_damage && true_damage <= d_hi,
                    "max_hp={max_hp}: display {pre_pct}%→{post_pct}% (raw {pre_raw}→{post_raw}), \
                     true damage {true_damage} outside band [{d_lo}, {d_hi}]"
                );
            }
        }
    }
}

/// The pre-S22 ±0.5%-of-delta band demonstrably excluded true damages for large-HP
/// defenders once the pre-hit HP was itself rounded (non-full). This pins one
/// concrete counterexample so the old formula cannot silently return.
#[test]
fn test_s22_old_band_counterexample_now_covered() {
    use crate::information::inference::percent_bucket;
    use crate::simulator::helpers::hp_to_percent;

    let max_hp: u16 = 362; // e.g. max-HP Blissey at level 50
    // Find a (pre_raw, post_raw) pair whose true damage violates the OLD band.
    let mut found = None;
    'outer: for pre_raw in 1..max_hp {
        for post_raw in 1..pre_raw {
            let pre_pct = hp_to_percent(pre_raw, max_hp);
            let post_pct = hp_to_percent(post_raw, max_hp);
            if post_pct >= pre_pct || pre_pct == 100 {
                continue;
            }
            let delta = (pre_pct - post_pct) as f64;
            let old_lo = ((delta - 0.5) * max_hp as f64 / 100.0).floor().max(1.0) as u16;
            let old_hi = ((delta + 0.5) * max_hp as f64 / 100.0).ceil() as u16;
            let true_damage = pre_raw - post_raw;
            if true_damage < old_lo || true_damage > old_hi {
                found = Some((pre_raw, post_raw, pre_pct, post_pct, true_damage));
                break 'outer;
            }
        }
    }
    let (pre_raw, post_raw, pre_pct, post_pct, true_damage) =
        found.expect("the old ±0.5%-of-delta band must have at least one gap at 362 max HP");

    // The new bucket-derived band must cover it.
    let (pre_lo, pre_hi) = percent_bucket(pre_pct, max_hp).unwrap();
    let (post_lo, post_hi) = percent_bucket(post_pct, max_hp).unwrap();
    let d_lo = pre_lo.saturating_sub(post_hi).max(1);
    let d_hi = pre_hi.saturating_sub(post_lo);
    assert!(
        d_lo <= true_damage && true_damage <= d_hi,
        "raw {pre_raw}→{post_raw} (display {pre_pct}%→{post_pct}%): true damage \
         {true_damage} must be inside the new band [{d_lo}, {d_hi}]"
    );
}

// ── Regression: S20 — Choice exclusion must not fire on a transferred Choice item ──

/// P2's mon uses Tackle, receives a Choice Scarf via Trick (ItemGained), then legally
/// selects a different move — Choice lock only binds from the first move used while
/// holding the item. Before the S20 fix, `pass1_choice_exclusion` saw two distinct
/// moves this stint and tried to exclude ChoiceScarf from the mon's now-Known(Scarf)
/// item — an unconditional contradiction panic on a legal game sequence.
#[test]
fn test_s20_choice_exclusion_skipped_for_tricked_item() {
    let p1_mon = unknown_mon_species(Species::Garchomp);
    let p2_mon = unknown_mon_species(Species::Snorlax);
    let state = battle_1v1(p1_mon, p2_mon);

    let result = apply(
        state,
        vec![
            event(EventKind::MoveUsed { user: p2(0), move_used: PokemonMove::Tackle, targets: vec![p1(0)] }),
            event(EventKind::ItemGained { slot: p2(0), item: Item::ChoiceScarf }),
            event(EventKind::MoveUsed { user: p2(0), move_used: PokemonMove::BodySlam, targets: vec![p1(0)] }),
        ],
    );
    assert!(
        matches!(&result.p2_active_mons[0].item, Unknown::Known(i) if *i == Item::ChoiceScarf),
        "the transferred Choice Scarf must survive the multi-move stint"
    );
}

// ── Regression: S17 — conditional SpeedComparisons must not propagate bounds ────

/// A slow mon with a possible Quick Claw moves before our exactly-known fast mon.
/// The emitted clause is `[SpeedComparison ∨ HasItem(QuickClaw) ∨ …]` — the order is
/// fully explained by a Quick Claw proc, so the comparison must NOT be enforced as a
/// hard Spe bound. Before the S17 fix, `collect_speed_comparisons` harvested
/// `SpeedComparison` literals out of multi-literal clauses and
/// `propagate_speed_comparisons` enforced them unconditionally: here that raised the
/// slow mon's min Spe from its species floor to 150, excluding every true
/// (slower-with-Quick-Claw) world — and panicking outright whenever the forced min
/// exceeded the species' maximum.
#[test]
fn test_s17_conditional_speed_comparison_not_propagated() {
    let mut move_dex = HashMap::new();
    move_dex.insert(PokemonMove::Earthquake, normal_physical_move(PokemonMove::Earthquake, 100));
    move_dex.insert(PokemonMove::DragonClaw, normal_physical_move(PokemonMove::DragonClaw, 80));

    // P1: our mon with exactly-known Spe = 150.
    let mut p1_mon = unknown_mon_species(Species::Garchomp);
    p1_mon.min_stats[5] = 150;
    p1_mon.max_stats[5] = 150;

    // P2: item completely unknown → Quick Claw is a live escape.
    let p2_mon = unknown_mon_species(Species::Snorlax);
    let p2_min_before = p2_mon.min_stats[5];

    let state = battle_1v1(p1_mon, p2_mon);
    // P2 moves first (Quick Claw proc), P1 second — same priority bracket.
    let result = apply_ex(
        state,
        vec![
            event(EventKind::MoveUsed { user: p2(0), move_used: PokemonMove::Earthquake, targets: vec![p1(0)] }),
            event(EventKind::MoveUsed { user: p1(0), move_used: PokemonMove::DragonClaw, targets: vec![p2(0)] }),
        ],
        HashMap::new(),
        move_dex,
    );

    let p2_after = &result.p2_active_mons[0];
    assert_eq!(
        p2_after.min_stats[5], p2_min_before,
        "conditional SpeedComparison (live QuickClaw escape) must not raise the slow \
         mon's min Spe"
    );
    // The conditional clause itself must survive for BCP to resolve later.
    let clause_present = result.predicates.iter().any(|clause| {
        clause.len() > 1
            && clause.iter().any(|s| matches!(s, Statement::SpeedComparison { .. }))
            && clause.iter().any(|s| matches!(s, Statement::HasItem { item: Item::QuickClaw, .. }))
    });
    assert!(clause_present, "the conditional clause must remain in the predicate store");
}

/// Companion: once every escape in the clause is excluded, BCP collapses it to a unit
/// SpeedComparison and the bound DOES propagate — the S17 fix must not disable the
/// intended unit-clause path.
#[test]
fn test_s17_unit_speed_comparison_still_propagates() {
    let mut move_dex = HashMap::new();
    move_dex.insert(PokemonMove::Earthquake, normal_physical_move(PokemonMove::Earthquake, 100));
    move_dex.insert(PokemonMove::DragonClaw, normal_physical_move(PokemonMove::DragonClaw, 80));

    let mut p1_mon = unknown_mon_species(Species::Garchomp);
    p1_mon.min_stats[5] = 100;
    p1_mon.max_stats[5] = 100;

    // Exclude every possible escape on both sides → the emitted clause is unit.
    let mut p2_mon = unknown_mon_species(Species::Snorlax);
    p2_mon.item = Unknown::Not(vec![
        Item::QuickClaw, Item::ChoiceScarf, Item::CustapBerry,
    ]);
    p2_mon.possible_abilities = Unknown::Not(vec![
        Ability::QuickDraw, Ability::Prankster, Ability::GaleWings, Ability::Triage,
        Ability::SwiftSwim, Ability::Chlorophyll, Ability::SandRush, Ability::SlushRush,
        Ability::SurgeSurfer, Ability::Unburden, Ability::QuickFeet,
    ]);
    let mut state = battle_1v1(p1_mon, p2_mon);
    // Slow-side escapes live on P1 (the second mover): Stall / Iron Ball / etc.
    state.p1_active_mons[0].possible_abilities = Unknown::Not(vec![Ability::Stall]);
    state.p1_active_mons[0].item = Unknown::Not(vec![
        Item::IronBall, Item::LaggingTail, Item::FullIncense,
    ]);
    state.p1_active_mons[0].min_stats[5] = 100;
    state.p1_active_mons[0].max_stats[5] = 100;

    let result = apply_ex(
        state,
        vec![
            event(EventKind::MoveUsed { user: p2(0), move_used: PokemonMove::Earthquake, targets: vec![p1(0)] }),
            event(EventKind::MoveUsed { user: p1(0), move_used: PokemonMove::DragonClaw, targets: vec![p1(0)] }),
        ],
        HashMap::new(),
        move_dex,
    );

    assert!(
        result.p2_active_mons[0].min_stats[5] >= 100,
        "unit SpeedComparison must still raise the fast mon's min Spe (got {})",
        result.p2_active_mons[0].min_stats[5]
    );
}

// ── Regression: S4 — Pass 4 must use speed-relevant state AS OF the comparison ──

/// A doubles turn where P1's Thunder Wave paralyzes a P2 mon mid-turn, and that
/// SAME mon then acts later in the SAME turn against its own ally: the paralysis
/// ×½ factor must be baked into the resulting Spe-bound tightening for that later
/// pairing.
///
/// Before the S4 fix, Pass 4 read `mon.status`/boosts live from `state` at its
/// call time (either before the turn's events were walked, when the paralysis
/// hadn't happened yet, or after the whole turn including EndOfTurn, which can
/// also disagree) — never "as of the moment this specific pairing's order was
/// actually observed". That produced a numeric fast_mult/slow_mult that didn't
/// match what actually determined the order, which `propagate_speed_comparisons`
/// then uses to derive hard Spe bounds — a soundness risk (not just imprecision).
///
/// P2b (fast) is pinned to an exact known speed (60) so the tightening effect on
/// P2a's (slow) max Spe bound is directly observable: with the paralysis factor
/// correctly applied, `slow.max_stats[5] <= floor(60*8/4) = 120`. Without it (the
/// pre-fix bug, using the neutral 1:1 ratio), `slow.max_stats[5] <= floor(60*4/4)
/// = 60` — an unsound over-tightening that excludes any true Spe in (60, 120].
#[test]
fn test_pass4_speed_bound_reflects_mid_turn_paralysis() {
    let mut thunder_wave = normal_physical_move(PokemonMove::ThunderWave, 0);
    thunder_wave.category = MoveCategory::Status;
    thunder_wave.accuracy = AccuracyType::Percent(100);
    let splash = normal_physical_move(PokemonMove::Splash, 0);

    let mut move_dex = HashMap::new();
    move_dex.insert(PokemonMove::ThunderWave, thunder_wave);
    move_dex.insert(PokemonMove::Splash, splash);

    // P1a: known, no Quick Claw/Quick Draw (clean SpeedComparisons, no disjuncts).
    let mut p1a = unknown_mon_species(Species::Pikachu);
    p1a.item = Unknown::Not(vec![Item::QuickClaw]);
    p1a.possible_abilities = Unknown::Not(vec![Ability::QuickDraw]);
    let mut p1b = unknown_mon_species(Species::Pikachu);
    p1b.item = Unknown::Not(vec![Item::QuickClaw]);
    p1b.possible_abilities = Unknown::Not(vec![Ability::QuickDraw]);

    // P2a: the mon that gets paralyzed mid-turn (default wide Snorlax Spe range).
    // P2b: its ally, pinned to an exact Spe of 60, moves right before it.
    // Every escape disjunct on the (fast=P2b, slow=P2a) pairing is excluded so the
    // emitted clause is a UNIT SpeedComparison — since the S17 fix, only unit
    // clauses propagate hard Spe bounds.
    let mut p2a = unknown_mon_species(Species::Snorlax);
    p2a.possible_abilities = Unknown::Not(vec![Ability::QuickFeet, Ability::Stall]);
    p2a.item = Unknown::Not(vec![Item::IronBall, Item::LaggingTail, Item::FullIncense]);
    let natural_max_spe = p2a.max_stats[5];

    let mut p2b = unknown_mon_species(Species::Snorlax);
    p2b.possible_abilities = Unknown::Not(vec![Ability::QuickFeet, Ability::QuickDraw]);
    p2b.item = Unknown::Not(vec![Item::QuickClaw, Item::ChoiceScarf]);
    p2b.min_stats[5] = 60;
    p2b.max_stats[5] = 60;
    p2b.min_pre_nature_stat[5] = 60;
    p2b.max_pre_nature_stat[5] = 60;

    let state = battle_nvn(vec![p1a, p1b], vec![p2a, p2b]);

    let p2a_idx_before = mon_idx_for_active_slot(&state, &p2(0)).unwrap();

    let result = apply_ex(
        state,
        vec![
            // P1a paralyzes P2a as the FIRST action this turn.
            event_with(
                EventKind::MoveUsed {
                    user: p1(0),
                    move_used: PokemonMove::ThunderWave,
                    targets: vec![p2(0)],
                },
                vec![event(EventKind::StatusInflicted { target: p2(0), status: Status::Paralysis })],
            ),
            // P2b moves next (same priority bracket)...
            event(EventKind::MoveUsed {
                user: p2(1),
                move_used: PokemonMove::Splash,
                targets: vec![],
            }),
            // ...then P2a moves LAST, now paralyzed — the pairing under test.
            event(EventKind::MoveUsed {
                user: p2(0),
                move_used: PokemonMove::Splash,
                targets: vec![],
            }),
        ],
        HashMap::new(),
        move_dex,
    );

    let p2a_after = get_mon_by_idx(&result, p2a_idx_before).unwrap();
    let expected_max = natural_max_spe.min(120);
    assert_eq!(
        p2a_after.max_stats[5], expected_max,
        "P2a's max Spe bound must reflect the paralysis-adjusted tightening \
         (floor(60*8/4) = 120, capped by its natural max {natural_max_spe}) = \
         {expected_max}. A bound of 60 here means Pass 4 used the stale (unparalyzed, \
         1:1 ratio) ordering instead of a snapshot as of the actual comparison point, \
         unsoundly excluding any true Spe in (60, {expected_max}]"
    );
}

// ── SpeedComparison propagation ───────────────────────────────────────────────

#[test]
fn test_speed_comparison_tightens_spe_bounds() {
    // Force a SpeedComparison directly into predicates and run BCP.
    // fast_idx=0 (P1 slot 0), slow_idx=1 (P2 slot 0), fast_mult=1, slow_mult=1.
    // If slow's min_stats[5] = 100, fast's min_stats[5] should be raised to ≥ 100.
    let mut p1_mon = unknown_mon_species(Species::Pikachu);
    p1_mon.min_stats[5] = 50;
    p1_mon.max_stats[5] = 200;

    let mut p2_mon = unknown_mon_species(Species::Snorlax);
    p2_mon.min_stats[5] = 100;
    p2_mon.max_stats[5] = 150;

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
    // fast's min_stats[5] should be raised to ≥ slow's min_stats[5] = 100.
    assert!(
        result.p1_active_mons[0].min_stats[5] >= 100,
        "SpeedComparison must raise fast mon's min Spe to ≥ slow mon's min ({})",
        result.p1_active_mons[0].min_stats[5]
    );
}

/// The symmetric BCP branch (previously untested): SpeedComparison must LOWER the
/// slow mon's max Spe to the fast mon's max.
#[test]
fn test_speed_comparison_lowers_slow_max_spe() {
    let mut p1_mon = unknown_mon_species(Species::Pikachu);
    p1_mon.min_stats[5] = 50;
    p1_mon.max_stats[5] = 120;

    let mut p2_mon = unknown_mon_species(Species::Snorlax);
    p2_mon.min_stats[5] = 40;
    p2_mon.max_stats[5] = 200;

    let mut state = battle_1v1(p1_mon, p2_mon);
    state.predicates.push(vec![Statement::SpeedComparison {
        fast_idx: 0,
        slow_idx: 1,
        fast_mult: 1,
        slow_mult: 1,
    }]);

    let result = apply(state, vec![]);
    assert!(
        result.p2_active_mons[0].max_stats[5] <= 120,
        "SpeedComparison must lower slow mon's max Spe to ≤ fast mon's max ({})",
        result.p2_active_mons[0].max_stats[5]
    );
}

// ── Zoroark parallel-hypothesis regression tests ────────────────────────────────
//
// These replace the old species-widening (`Possibly([shown, Zoroark])`) model's
// tests. Under the current model `possible_species` always stays pinned (`Known`)
// — the Zoroark ambiguity lives entirely in a separate, full `UnknownPokemonState`
// hypothesis at `possible_illusion_state`. `seed_zoroark_hypothesis_on` below
// mimics what `seed_illusion_hypotheses` (`unknowns.rs`) does at the real
// team-preview→battle transition, so these tests can exercise `pass1_switch` /
// `bench_outgoing_mon` / promotion directly without a full team-preview setup;
// `test_zoroark_possibly_in_back_from_team_preview` below exercises the real
// seeding path end-to-end.

/// Attach a fresh Zoroark hypothesis to `host` from `zoroark`'s own tracked state,
/// exactly as `unknowns::seed_illusion_hypothesis_for` would at team preview.
fn seed_zoroark_hypothesis_on(host: &mut UnknownPokemonState, zoroark: &UnknownPokemonState) {
    host.possible_illusion_state = Some(Box::new(
        crate::information::unknowns::seed_illusion_hypothesis_for(host, zoroark),
    ));
}

/// A benched Garchomp (pre-seeded with a Zoroark hypothesis) and the side's real
/// benched Zoroark, ready to switch in.
fn garchomp_with_zoroark_hypothesis_and_baseline() -> (UnknownPokemonState, UnknownPokemonState) {
    let zoroark_back =
        UnknownPokemonState::from_opponent_species(Species::Zoroark, &HashMap::new(), 50);
    let mut garchomp_back =
        UnknownPokemonState::from_opponent_species(Species::Garchomp, &HashMap::new(), 50);
    seed_zoroark_hypothesis_on(&mut garchomp_back, &zoroark_back);
    (garchomp_back, zoroark_back)
}

fn switch_in(species: Species, slot: FieldSlot) -> InformationEvent {
    event(EventKind::Switch(SwitchState {
        disguise_species: None,
        max_hp: 0,
        slot,
        species,
        level: 50,
        hp: PokemonHP::Percent(100),
        status: None,
        tera_type: None,
    }))
}

/// Switching in a mon that carries a seeded Zoroark hypothesis must leave
/// `possible_species` pinned to `Known` (never widen to `Possibly`) while the
/// hypothesis rides along onto the active slot.
#[test]
fn test_zoroark_hypothesis_rides_onto_active_slot_species_stays_known() {
    let (garchomp_back, zoroark_back) = garchomp_with_zoroark_hypothesis_and_baseline();
    let mut state = battle_with_p2(vec![]);
    state.p2_known_back_mons = vec![garchomp_back];
    state.p2_possible_back_mons = vec![zoroark_back];
    state.p2_slot_conditions = vec![vec![]];
    state.p2_unresolved_zoroark_count = 1;

    let result = apply(state, vec![switch_in(Species::Garchomp, p2(0))]);

    assert_eq!(
        result.p2_active_mons[0].possible_species,
        Unknown::Known(Species::Garchomp),
        "species must stay pinned to the shown species, never widen to Possibly"
    );
    assert!(
        result.p2_active_mons[0].possible_illusion_state.is_some(),
        "the seeded Zoroark hypothesis must carry onto the active slot"
    );
}

/// A move outside Zoroark's own learnset (but legal for the shown species) must
/// drop the hypothesis — this mon is confirmed NOT Zoroark — while the primary
/// identity is untouched.
#[test]
fn test_zoroark_learnset_contradiction_drops_hypothesis() {
    let (garchomp_back, zoroark_back) = garchomp_with_zoroark_hypothesis_and_baseline();
    let mut state = battle_with_p2(vec![]);
    state.p2_known_back_mons = vec![garchomp_back];
    state.p2_possible_back_mons = vec![zoroark_back];
    state.p2_slot_conditions = vec![vec![]];
    state.p2_unresolved_zoroark_count = 1;
    let state = apply(state, vec![switch_in(Species::Garchomp, p2(0))]);
    assert!(state.p2_active_mons[0].possible_illusion_state.is_some());

    let mut learnset_dex: HashMap<Species, HashSet<PokemonMove>> = HashMap::new();
    learnset_dex.insert(Species::Garchomp, [PokemonMove::Earthquake].into_iter().collect());
    learnset_dex.insert(Species::Zoroark, [PokemonMove::DarkPulse].into_iter().collect());
    let config = InferenceConfig { learnset_dex, ev_total_cap: None, ..Default::default() };

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

    assert_eq!(
        result.p2_active_mons[0].possible_species,
        Unknown::Known(Species::Garchomp),
        "primary identity is unaffected by the hypothesis rejection"
    );
    assert!(
        result.p2_active_mons[0].possible_illusion_state.is_none(),
        "a move outside Zoroark's learnset must drop the hypothesis"
    );
}

/// A move outside the SHOWN species' own learnset, but legal for Zoroark, must
/// PROMOTE the hypothesis: the mon is confirmed to secretly be Zoroark.
#[test]
fn test_zoroark_learnset_promotes_when_primary_impossible() {
    let (garchomp_back, zoroark_back) = garchomp_with_zoroark_hypothesis_and_baseline();
    let mut state = battle_with_p2(vec![]);
    state.p2_known_back_mons = vec![garchomp_back];
    state.p2_possible_back_mons = vec![zoroark_back];
    state.p2_slot_conditions = vec![vec![]];
    state.p2_unresolved_zoroark_count = 1;
    let state = apply(state, vec![switch_in(Species::Garchomp, p2(0))]);

    let mut learnset_dex: HashMap<Species, HashSet<PokemonMove>> = HashMap::new();
    learnset_dex.insert(Species::Garchomp, [PokemonMove::Earthquake].into_iter().collect());
    learnset_dex.insert(Species::Zoroark, [PokemonMove::DarkPulse].into_iter().collect());
    let config = InferenceConfig { learnset_dex, ev_total_cap: None, ..Default::default() };

    let result = apply_with_config(
        state,
        vec![event(EventKind::MoveUsed {
            user: p2(0),
            move_used: PokemonMove::DarkPulse,
            targets: vec![p1(0)],
        })],
        HashMap::new(),
        HashMap::new(),
        config,
    );

    assert_eq!(
        result.p2_active_mons[0].possible_species,
        Unknown::Known(Species::Zoroark),
        "a move only Zoroark can know must promote the hypothesis to primary"
    );
    assert!(
        result.p2_active_mons[0].possible_illusion_state.is_none(),
        "promotion clears the (now-redundant) hypothesis slot"
    );
    // Side-wide bookkeeping: Zoroark is now positively located.
    assert_eq!(
        result.p2_unresolved_zoroark_count, 0,
        "resolve_zoroark_globally must decrement the unresolved count on promotion"
    );
}

/// Regression test for the `random_doubles_battles_are_sound` fuzz-test failure:
/// Illusion re-activates on EVERY switch-in with no "already revealed"
/// suppression (`simulator::helpers::compute_illusion_disguise`), so a Zoroark
/// resolved once can switch out and later re-enter disguised as a DIFFERENT
/// decoy. `resolve_zoroark_globally` only ever decrements
/// `p{side}_unresolved_zoroark_count`, so without `rearm_zoroark_on_side` the
/// count stayed at 0 forever after the first resolution — the re-disguised
/// slot's next signature move then hard-panicked in
/// `check_move_legal_for_species` instead of promoting, since no hypothesis was
/// left anywhere on the side to route the panic through. This drives the exact
/// sequence: Zoroark resolves as Garchomp, switches out, switches back in as
/// Milotic (a different decoy), and must re-promote on its next signature move.
#[test]
fn test_zoroark_repromotes_after_switching_out_and_back_in_as_different_decoy() {
    let (garchomp_back, zoroark_back) = garchomp_with_zoroark_hypothesis_and_baseline();
    let milotic_back =
        UnknownPokemonState::from_opponent_species(Species::Milotic, &HashMap::new(), 50);
    let mut state = battle_with_p2(vec![]);
    state.p2_known_back_mons = vec![garchomp_back, milotic_back];
    state.p2_possible_back_mons = vec![zoroark_back];
    state.p2_slot_conditions = vec![vec![]];
    state.p2_unresolved_zoroark_count = 1;

    let mut learnset_dex: HashMap<Species, HashSet<PokemonMove>> = HashMap::new();
    learnset_dex.insert(Species::Garchomp, [PokemonMove::Earthquake].into_iter().collect());
    learnset_dex.insert(Species::Milotic, [PokemonMove::Scald].into_iter().collect());
    learnset_dex.insert(Species::Zoroark, [PokemonMove::NastyPlot].into_iter().collect());
    let make_config = || InferenceConfig {
        learnset_dex: learnset_dex.clone(),
        ev_total_cap: None,
        ..Default::default()
    };

    // Zoroark enters disguised as Garchomp, then reveals itself with a move only
    // Zoroark can learn.
    let state = apply_with_config(
        state,
        vec![
            switch_in(Species::Garchomp, p2(0)),
            event(EventKind::MoveUsed {
                user: p2(0),
                move_used: PokemonMove::NastyPlot,
                targets: vec![p1(0)],
            }),
        ],
        HashMap::new(),
        HashMap::new(),
        make_config(),
    );
    assert_eq!(state.p2_active_mons[0].possible_species, Unknown::Known(Species::Zoroark));
    assert_eq!(
        state.p2_unresolved_zoroark_count, 0,
        "first promotion must resolve the side's Zoroark"
    );

    // The now-revealed Zoroark switches out; Milotic — a DIFFERENT decoy than the
    // one it was resolved from — switches in. Illusion re-activates: this slot is
    // secretly Zoroark again.
    let state = apply_with_config(
        state,
        vec![switch_in(Species::Milotic, p2(0))],
        HashMap::new(),
        HashMap::new(),
        make_config(),
    );
    assert_eq!(
        state.p2_unresolved_zoroark_count, 1,
        "the resolved Zoroark switching back out must re-arm tracking, not leave it at 0 forever"
    );
    assert!(
        state.p2_active_mons[0].possible_illusion_state.is_some(),
        "the newly-arrived decoy slot must carry a freshly re-seeded hypothesis"
    );

    // "Milotic" uses Nasty Plot — illegal for Milotic, legal for Zoroark. Before
    // the fix this hard-panicked in `check_move_legal_for_species`.
    let result = apply_with_config(
        state,
        vec![event(EventKind::MoveUsed {
            user: p2(0),
            move_used: PokemonMove::NastyPlot,
            targets: vec![p1(0)],
        })],
        HashMap::new(),
        HashMap::new(),
        make_config(),
    );

    assert_eq!(
        result.p2_active_mons[0].possible_species,
        Unknown::Known(Species::Zoroark),
        "the re-disguised slot must re-promote to Zoroark instead of panicking"
    );
    assert!(result.p2_active_mons[0].possible_illusion_state.is_none());
    assert_eq!(result.p2_unresolved_zoroark_count, 0);

    // No stale duplicate bench entry left claiming to also be Zoroark (the second
    // promotion's `remove_stale_zoroark_bench_duplicate` follow-up).
    let bench_zoroark_count = combined_back(&result, &Player::P2)
        .into_iter()
        .filter(|m| matches!(&m.possible_species, Unknown::Known(Species::Zoroark)))
        .count();
    assert_eq!(
        bench_zoroark_count, 0,
        "no stale duplicate Zoroark bench entry should remain after the second promotion"
    );

    // Both decoys — Garchomp from the first reveal, Milotic from the second —
    // must be restored to the bench, not lost.
    let bench_species: Vec<Species> = combined_back(&result, &Player::P2)
        .into_iter()
        .filter_map(|m| match &m.possible_species {
            Unknown::Known(s) => Some(s.clone()),
            _ => None,
        })
        .collect();
    assert!(bench_species.contains(&Species::Garchomp), "the first decoy must be restored");
    assert!(bench_species.contains(&Species::Milotic), "the second decoy must be restored");
}

/// Switch-out must PERSIST the hypothesis (bench it alongside the primary), not
/// discard it — this reverses the old S29 discard-on-switch-out behavior, which
/// is exactly the "information must persist even if it switches out" requirement.
#[test]
fn test_zoroark_switch_out_persists_hypothesis() {
    let (garchomp_back, zoroark_back) = garchomp_with_zoroark_hypothesis_and_baseline();
    let mut state = battle_with_p2(vec![]);
    state.p2_known_back_mons = vec![garchomp_back];
    state.p2_possible_back_mons = vec![zoroark_back];
    state.p2_slot_conditions = vec![vec![]];
    state.p2_unresolved_zoroark_count = 1;

    let result = apply(
        state,
        vec![
            switch_in(Species::Garchomp, p2(0)),
            // Switches back out for Snorlax before the disguise ever resolves.
            switch_in(Species::Snorlax, p2(0)),
        ],
    );

    let benched_garchomp = result
        .p2_known_back_mons
        .iter()
        .find(|m| matches!(&m.possible_species, Unknown::Known(Species::Garchomp)));
    let benched_garchomp = benched_garchomp.expect(
        "the real Garchomp must be benched (not discarded) after switching out",
    );
    assert!(
        benched_garchomp.possible_illusion_state.is_some(),
        "the hypothesis must persist onto the benched entry, not be discarded"
    );
}

/// A mon that benches with a live hypothesis and later returns to the field must
/// resume its OWN accumulated hypothesis (not a fresh, re-seeded one) — this is
/// "own-hypothesis resume." Demonstrated by narrowing the hypothesis's item while
/// active, benching it, bringing back a DIFFERENT mon, then switching the original
/// back in and confirming the narrowed item survived the round trip.
#[test]
fn test_zoroark_own_hypothesis_resumes_on_return() {
    let (mut garchomp_back, zoroark_back) = garchomp_with_zoroark_hypothesis_and_baseline();
    // Narrow the hypothesis's item bound before it's even switched in, to give the
    // round trip something distinguishing to check for.
    garchomp_back.possible_illusion_state.as_mut().unwrap().item = Unknown::Known(Item::Leftovers);
    let mut snorlax_back = unknown_mon_species(Species::Snorlax);
    let mut state = battle_with_p2(vec![]);
    state.p2_known_back_mons = vec![garchomp_back, snorlax_back.clone()];
    state.p2_possible_back_mons = vec![zoroark_back];
    state.p2_slot_conditions = vec![vec![]];
    state.p2_unresolved_zoroark_count = 1;

    let result = apply(
        state,
        vec![
            switch_in(Species::Garchomp, p2(0)),
            switch_in(Species::Snorlax, p2(0)), // Garchomp benches, hypothesis rides along
            switch_in(Species::Garchomp, p2(0)), // Garchomp returns
        ],
    );

    let hyp = result.p2_active_mons[0]
        .possible_illusion_state
        .as_ref()
        .expect("the returning mon must still carry its own hypothesis");
    assert_eq!(
        hyp.item,
        Unknown::Known(Item::Leftovers),
        "the RESUMED hypothesis must be the same one narrowed before benching, \
         not a fresh re-seeded copy"
    );
}

/// `IllusionEnded` (a direct-damage disguise break) must promote the live
/// hypothesis wholesale and restore the discarded primary identity ("Garchomp")
/// to `possible_back` at a fresh baseline — it was never really on the field.
#[test]
fn test_zoroark_illusion_ended_promotes_and_restores_decoy() {
    let (garchomp_back, zoroark_back) = garchomp_with_zoroark_hypothesis_and_baseline();
    let mut state = battle_with_p2(vec![]);
    state.p2_known_back_mons = vec![garchomp_back];
    state.p2_possible_back_mons = vec![zoroark_back];
    state.p2_slot_conditions = vec![vec![]];
    state.p2_unresolved_zoroark_count = 1;
    let state = apply(state, vec![switch_in(Species::Garchomp, p2(0))]);

    let result = apply(
        state,
        vec![event(EventKind::IllusionEnded { slot: p2(0), actual_species: Species::Zoroark })],
    );

    assert_eq!(
        result.p2_active_mons[0].possible_species,
        Unknown::Known(Species::Zoroark),
        "IllusionEnded must promote the slot to the true (Zoroark) identity"
    );
    assert!(
        result.p2_active_mons[0].possible_illusion_state.is_none(),
        "promotion clears the hypothesis slot"
    );
    assert!(
        combined_back_species(&result, &Player::P2).contains(&Species::Garchomp),
        "the discarded decoy identity (Garchomp) must be restored to the bench — \
         it was never actually on the field; bench species = {:?}",
        combined_back_species(&result, &Player::P2)
    );
    assert_eq!(
        result.p2_unresolved_zoroark_count, 0,
        "the side's Zoroark is now positively located"
    );
}

/// TODO.md regression, open-sheet ground truth: when a disguised Zoroark's
/// disguise breaks, the discarded decoy identity (e.g. Dragonite) is restored
/// to `possible_back` via `restore_discarded_primary_to_bench` — but under an
/// open team sheet, that decoy's item/moves/ability/nature were already fully
/// `Known` from turn 0 (`team_preview_open_sheet_from_perspective`). Rebuilding
/// it species-only (`from_opponent_species`, the pre-fix behavior) regresses a
/// fully-known mon to "no information" the moment its disguise is seen through
/// — exactly the bug reported in TODO.md ("it displays that we have no
/// information about that mon, but it should use the teamsheet information").
/// This drives the real lifecycle end-to-end with real dex data and asserts the
/// restored decoy — and the same physical mon once it later switches in for
/// real — still carries its open-sheet set.
#[test]
fn test_zoroark_illusion_ended_restores_decoy_with_open_sheet_data() {
    use crate::information::unknowns::{InformationMode, UnknownTeamPreviewState};
    use crate::state::pokemon::{build_pokemon_state, Nature};
    use crate::tests::simuilator_test_helpers::{move_dex, pokemon_dex};

    let pd = pokemon_dex();
    let md = move_dex();

    let p1_lead = build_pokemon_state(
        Species::Snorlax, pd, md, Some(50), None, None, None, None, None, None, None, None, true,
    );
    let zoroark = build_pokemon_state(
        Species::Zoroark, pd, md, Some(50),
        Some([Some(PokemonMove::NastyPlot), Some(PokemonMove::DarkPulse), None, None]),
        None, Some(Ability::Illusion), Some(Nature::Timid), Some(Item::ChoiceSpecs),
        None, None, None, true,
    );
    let dragonite = build_pokemon_state(
        Species::Dragonite, pd, md, Some(50),
        Some([Some(PokemonMove::DragonDance), Some(PokemonMove::ExtremeSpeed), None, None]),
        None, Some(Ability::Multiscale), Some(Nature::Adamant), Some(Item::HeavyDutyBoots),
        None, None, None, true,
    );
    let corviknight = build_pokemon_state(
        Species::Corviknight, pd, md, Some(50), None, None, None, None, None, None, None, None, true,
    );

    let preview_state = crate::information::unknowns::UnknownMatchState::team_preview_open_sheet_from_perspective(
        Player::P1,
        &[p1_lead],
        &[zoroark, dragonite, corviknight],
        pd,
        1,
        3,
        50,
        InformationMode::OpenTeamSheet,
        true,
    );
    let preview: UnknownTeamPreviewState = match preview_state {
        crate::information::unknowns::UnknownMatchState::TeamPreview(tp) => tp,
        _ => panic!("expected TeamPreview state"),
    };
    let seeded = preview.into_battle_state(Player::P1, &[0], &[], &[1], &[0, 2]);
    assert_eq!(
        seeded.p2_unresolved_zoroark_count, 1,
        "team preview must detect the one real Zoroark on P2's roster"
    );

    // P2 leads Zoroark, DISGUISED as Dragonite — the observer's event stream
    // (perspective-gated, as the real simulator would emit it) carries the
    // shown species, not the true one.
    let after_lead = apply_real_dex(
        seeded,
        vec![event(EventKind::SimultaneousSwitch {
            switches: vec![
                SwitchState { disguise_species: None, max_hp: 0,
                    slot: p1(0), species: Species::Snorlax, level: 50,
                    hp: PokemonHP::Number(200), status: None, tera_type: None,
                },
                SwitchState { disguise_species: None, max_hp: 0,
                    slot: p2(0), species: Species::Dragonite, level: 50,
                    hp: PokemonHP::Percent(100), status: None, tera_type: None,
                },
            ],
        })],
        pd,
        md,
    );

    // A real damaging hit breaks the disguise.
    let after_reveal = apply_real_dex(
        after_lead,
        vec![event(EventKind::IllusionEnded { slot: p2(0), actual_species: Species::Zoroark })],
        pd,
        md,
    );

    let restored_dragonite = combined_back(&after_reveal, &Player::P2)
        .into_iter()
        .find(|m| matches!(&m.possible_species, Unknown::Known(Species::Dragonite)))
        .expect("the discarded Dragonite decoy must be restored to the bench");
    assert_eq!(
        restored_dragonite.item,
        Unknown::Known(Item::HeavyDutyBoots),
        "open-sheet item must survive the restore, not regress to fully unknown"
    );
    assert_eq!(
        restored_dragonite.possible_abilities,
        Unknown::Known(Ability::Multiscale),
        "open-sheet ability must survive the restore"
    );
    assert!(
        restored_dragonite.known_moves.contains(&Some(PokemonMove::DragonDance))
            && restored_dragonite.known_moves.contains(&Some(PokemonMove::ExtremeSpeed)),
        "open-sheet moves must survive the restore, got {:?}",
        restored_dragonite.known_moves
    );

    // The same physical mon later switches in for real — the display-facing
    // symptom from TODO.md — and must show that same preserved set, not
    // "no information".
    let after_return = apply_real_dex(
        after_reveal,
        vec![switch_in(Species::Dragonite, p2(0))],
        pd,
        md,
    );
    let active_dragonite = &after_return.p2_active_mons[0];
    assert_eq!(active_dragonite.possible_species, Unknown::Known(Species::Dragonite));
    assert_eq!(active_dragonite.item, Unknown::Known(Item::HeavyDutyBoots));
    assert_eq!(active_dragonite.possible_abilities, Unknown::Known(Ability::Multiscale));
}

/// Live-server regression found via end-to-end verification (not by static
/// analysis): promotion can happen via move-legality mirroring — a move the
/// shown species can't learn but Zoroark can — WITHOUT the disguise ever
/// visibly breaking (`IllusionEnded` only fires on direct damage or an ability
/// change; a status move revealing illegality never damages anyone). Driving
/// this exact sequence against the real Axum server (Zoroark-Hisui disguised as
/// Dragonite, using Nasty Plot — legal for Zoroark, not for Dragonite) showed
/// the decoy vanishing from the tracked roster entirely: `restore_discarded_primary_to_bench`
/// was only ever called from the `IllusionEnded` handler, so a promotion that
/// happens via any OTHER path (move-legality here; the same gap exists for the
/// Pass 3/Pass 5 stat-tightening backstop and item-reveal mirroring) never
/// restored the decoy — and by the time (if ever) `IllusionEnded` later fires,
/// the handler's own "what was discarded" capture reads the ALREADY-promoted
/// Zoroark identity, not the decoy, so its restore is skipped too. This is
/// `finish_illusion_promotion_restore`'s fix: every promotion site now restores
/// the decoy at the moment it actually happens, not only on an explicit
/// `IllusionEnded` reveal.
#[test]
fn test_zoroark_move_legality_promotion_restores_decoy_without_illusion_ended() {
    use crate::state::pokemon::{build_pokemon_state, Nature};
    use crate::tests::simuilator_test_helpers::{move_dex, pokemon_dex};

    let pd = pokemon_dex();
    let md = move_dex();

    let p1_lead = build_pokemon_state(
        Species::Snorlax, pd, md, Some(50), None, None, None, None, None, None, None, None, true,
    );
    let zoroark = build_pokemon_state(
        Species::Zoroark, pd, md, Some(50),
        Some([Some(PokemonMove::NastyPlot), Some(PokemonMove::DarkPulse), None, None]),
        None, Some(Ability::Illusion), Some(Nature::Timid), Some(Item::ChoiceSpecs),
        None, None, None, true,
    );
    let dragonite = build_pokemon_state(
        Species::Dragonite, pd, md, Some(50),
        Some([Some(PokemonMove::DragonDance), Some(PokemonMove::ExtremeSpeed), None, None]),
        None, Some(Ability::Multiscale), Some(Nature::Adamant), Some(Item::HeavyDutyBoots),
        None, None, None, true,
    );
    let corviknight = build_pokemon_state(
        Species::Corviknight, pd, md, Some(50), None, None, None, None, None, None, None, None, true,
    );

    let preview_state = crate::information::unknowns::UnknownMatchState::team_preview_open_sheet_from_perspective(
        Player::P1,
        &[p1_lead],
        &[zoroark, dragonite, corviknight],
        pd,
        1,
        3,
        50,
        crate::information::unknowns::InformationMode::OpenTeamSheet,
        true,
    );
    let preview: crate::information::unknowns::UnknownTeamPreviewState = match preview_state {
        crate::information::unknowns::UnknownMatchState::TeamPreview(tp) => tp,
        _ => panic!("expected TeamPreview state"),
    };
    let seeded = preview.into_battle_state(Player::P1, &[0], &[], &[1, 2], &[0]);
    assert_eq!(seeded.p2_unresolved_zoroark_count, 1);

    // P2 leads Zoroark, disguised as Dragonite.
    let after_lead = apply_real_dex(
        seeded,
        vec![event(EventKind::SimultaneousSwitch {
            switches: vec![
                SwitchState { disguise_species: None, max_hp: 0,
                    slot: p1(0), species: Species::Snorlax, level: 50,
                    hp: PokemonHP::Number(200), status: None, tera_type: None,
                },
                SwitchState { disguise_species: None, max_hp: 0,
                    slot: p2(0), species: Species::Dragonite, level: 50,
                    hp: PokemonHP::Percent(100), status: None, tera_type: None,
                },
            ],
        })],
        pd, md,
    );

    // Nasty Plot: illegal for Dragonite, legal for Zoroark — promotes via
    // move-legality mirroring alone. `IllusionEnded` is never emitted in this
    // test at all — the point is that the restore must not depend on it.
    let mut learnset_dex: HashMap<Species, HashSet<PokemonMove>> = HashMap::new();
    learnset_dex.insert(
        Species::Dragonite,
        [PokemonMove::DragonDance, PokemonMove::ExtremeSpeed].into_iter().collect(),
    );
    learnset_dex.insert(
        Species::Zoroark,
        [PokemonMove::NastyPlot, PokemonMove::DarkPulse].into_iter().collect(),
    );
    let config = InferenceConfig { learnset_dex, ev_total_cap: None, ..Default::default() };
    let result = apply_information(
        UnknownMatchState::Battle(after_lead),
        &[event(EventKind::MoveUsed {
            user: p2(0),
            move_used: PokemonMove::NastyPlot,
            targets: vec![p2(0)],
        })],
        false,
        pd,
        md,
        &HashMap::new(),
        &config,
    );
    let UnknownMatchState::Battle(result) = result else { panic!("expected Battle state") };

    // Promotion happened via move-legality (not IllusionEnded).
    assert_eq!(result.p2_active_mons[0].possible_species, Unknown::Known(Species::Zoroark));
    assert_eq!(result.p2_active_mons[0].item, Unknown::Known(Item::ChoiceSpecs));
    assert_eq!(result.p2_unresolved_zoroark_count, 0);

    // The decoy (Dragonite) must already be restored to the bench, with its
    // open-sheet set intact — the exact regression the live server exposed.
    let restored_dragonite = combined_back(&result, &Player::P2)
        .into_iter()
        .find(|m| matches!(&m.possible_species, Unknown::Known(Species::Dragonite)))
        .expect(
            "the decoy must be restored to the bench immediately on promotion, \
             not only when/if IllusionEnded later fires",
        );
    assert_eq!(
        restored_dragonite.item,
        Unknown::Known(Item::HeavyDutyBoots),
        "open-sheet item must survive this promotion path too"
    );
    assert_eq!(restored_dragonite.possible_abilities, Unknown::Known(Ability::Multiscale));
}

/// S49: found via a 300+-seed doubles fuzz sweep (`random_battle_tests.rs`) as an
/// `ItemRevealed`-vs-`Not([..])` contradiction whose exclusion traced back to a
/// completely unrelated mon, sometimes on the OTHER side. Root cause: `mon_idx` for a
/// *benched* Pokémon is not a stable per-individual id — it's a live Vec-position
/// recomputed fresh from `MonSegments::ranges()` on every call, laid out as
/// `[p1_active, p2_active, p1_back, p2_back]`. `restore_discarded_primary_to_bench`
/// (called on every Illusion promotion, e.g. here via `IllusionEnded`) unconditionally
/// pushes the discarded shown identity onto the promoted mon's side's back bucket.
/// When that side's Illusion forme was the active lead from the start (no separate
/// bench placeholder for `remove_stale_zoroark_bench_duplicate` to remove in
/// exchange), the push is a NET +1 growth — which silently shifts every `mon_idx` in
/// the OTHER side's back segment by one, since P1's back segment sits immediately
/// before P2's in the flat layout. A CNF predicate recorded against an index at or
/// beyond that boundary — here, one pinning P2's own back mon to Leftovers — survives
/// the shift unchanged and gets force-committed by BCP against whatever now occupies
/// that (shifted) index instead: P1's own newly-restored decoy placeholder. This test
/// directly demonstrates the fix (`purge_facts_at_or_beyond_idx`, called from
/// `restore_discarded_primary_to_bench` right before the growing push): a predicate
/// referencing an index at/after the boundary must not survive the push.
#[test]
fn test_s49_illusion_promotion_bench_growth_does_not_shift_cross_side_predicate() {
    // P1's Zoroark is the active lead from turn 1, disguised as Garchomp, with NO
    // separate bench placeholder — exactly the case where the promotion's bench-push
    // isn't offset by a matching removal.
    let zoroark_baseline =
        UnknownPokemonState::from_opponent_species(Species::Zoroark, &HashMap::new(), 50);
    let mut garchomp_active =
        UnknownPokemonState::from_opponent_species(Species::Garchomp, &HashMap::new(), 50);
    seed_zoroark_hypothesis_on(&mut garchomp_active, &zoroark_baseline);

    let pikachu_active = UnknownPokemonState::from_opponent_species(Species::Pikachu, &HashMap::new(), 50);
    let blissey_back = UnknownPokemonState::from_opponent_species(Species::Blissey, &HashMap::new(), 50);

    let mut state = battle_nvn(vec![garchomp_active], vec![pikachu_active]);
    state.p1_unresolved_zoroark_count = 1;
    state.p2_known_back_mons = vec![blissey_back];

    // Flat layout before promotion: p1_active=[0,1), p2_active=[1,2), p1_back=[2,2)
    // (empty — Zoroark has no separate placeholder), p2_back=[2,3) — Blissey is idx 2.
    let blissey_idx_before = 2usize;
    assert_eq!(
        get_mon_by_idx(&state, blissey_idx_before).map(|m| &m.possible_species),
        Some(&Unknown::Known(Species::Blissey)),
        "test setup sanity: idx 2 must be Blissey before the promotion"
    );

    // Simulate an earlier pass having recorded a fact about Blissey (P2's back mon) —
    // e.g. an EOT-heal-sourced item disjunction that later collapsed to a unit clause.
    state.predicates.push(vec![Statement::HasItem { mon_idx: blissey_idx_before, item: Item::Leftovers }]);

    // The disguise breaks: P1's Zoroark is revealed, promoting the active slot and
    // pushing "Garchomp" (the discarded shown identity) onto P1's back bucket — a net
    // +1 growth with nothing to remove in exchange, shifting P2's back segment.
    let result = apply(
        state,
        vec![event(EventKind::IllusionEnded { slot: p1(0), actual_species: Species::Zoroark })],
    );

    assert_eq!(
        result.p1_active_mons[0].possible_species,
        Unknown::Known(Species::Zoroark),
        "IllusionEnded must promote P1's active slot to the true (Zoroark) identity"
    );
    let restored_garchomp = combined_back(&result, &Player::P1)
        .into_iter()
        .find(|m| matches!(&m.possible_species, Unknown::Known(Species::Garchomp)))
        .expect("the discarded Garchomp identity must be restored to P1's bench");

    // Flat layout after promotion: p1_back now holds the restored Garchomp decoy at
    // the SAME index (2) Blissey held before the shift. Without the fix, BCP would
    // force-commit the stale `HasItem(2, Leftovers)` clause onto THIS mon.
    assert_ne!(
        restored_garchomp.item,
        Unknown::Known(Item::Leftovers),
        "S49: a predicate recorded against P2's back mon before the promotion must not \
         survive the bench-growth shift and get force-committed onto P1's own restored \
         decoy placeholder just because it now occupies the same numeric index"
    );

    // Blissey itself — now shifted to idx 3 — must remain untouched by the stale
    // predicate (which referenced its OLD index, 2).
    let blissey_after = result
        .p2_known_back_mons
        .iter()
        .find(|m| matches!(&m.possible_species, Unknown::Known(Species::Blissey)))
        .expect("Blissey must still be tracked on P2's bench after the shift");
    assert_ne!(
        blissey_after.item,
        Unknown::Known(Item::Leftovers),
        "S49: Blissey's own item must not be spuriously resolved by a predicate that \
         only ever meant to reference its pre-shift index"
    );
}

/// F3 (TODO.md: "Make sure everything works with zoroark switching ... Tracking
/// zoroark should make sense over the course of the entire battle!"): a disguised
/// Zoroark that switches out WITHOUT its disguise ever breaking must bench under
/// its shown identity with its hypothesis intact (already covered synthetically by
/// `test_zoroark_switch_out_persists_hypothesis`/`test_zoroark_own_hypothesis_resumes_on_return`)
/// — this drives the same lifecycle with real open-sheet ground truth, across a
/// LONGER multi-switch sequence (three different replacements, then the disguised
/// mon returns), and asserts the strongest form of the TODO.md complaint directly:
/// at every step, every single P2 roster member's open-sheet item stays `Known` —
/// it must never regress to "no information" for ANY mon, at ANY point, no matter
/// how many switches have happened.
#[test]
fn test_zoroark_open_sheet_data_survives_full_battle_switch_sequence() {
    use crate::state::pokemon::{build_pokemon_state, Nature};
    use crate::tests::simuilator_test_helpers::{move_dex, pokemon_dex};

    let pd = pokemon_dex();
    let md = move_dex();

    let p1_lead = build_pokemon_state(
        Species::Snorlax, pd, md, Some(50), None, None, None, None, None, None, None, None, true,
    );
    let zoroark = build_pokemon_state(
        Species::Zoroark, pd, md, Some(50),
        Some([Some(PokemonMove::NastyPlot), Some(PokemonMove::DarkPulse), None, None]),
        None, Some(Ability::Illusion), Some(Nature::Timid), Some(Item::ChoiceSpecs),
        None, None, None, true,
    );
    let dragonite = build_pokemon_state(
        Species::Dragonite, pd, md, Some(50),
        Some([Some(PokemonMove::DragonDance), Some(PokemonMove::ExtremeSpeed), None, None]),
        None, Some(Ability::Multiscale), Some(Nature::Adamant), Some(Item::HeavyDutyBoots),
        None, None, None, true,
    );
    let corviknight = build_pokemon_state(
        Species::Corviknight, pd, md, Some(50),
        Some([Some(PokemonMove::BraveBird), Some(PokemonMove::Roost), None, None]),
        None, Some(Ability::Pressure), Some(Nature::Impish), Some(Item::Leftovers),
        None, None, None, true,
    );
    let milotic = build_pokemon_state(
        Species::Milotic, pd, md, Some(50),
        Some([Some(PokemonMove::Scald), Some(PokemonMove::Recover), None, None]),
        None, Some(Ability::MarvelScale), Some(Nature::Calm), Some(Item::AssaultVest),
        None, None, None, true,
    );

    let preview_state = crate::information::unknowns::UnknownMatchState::team_preview_open_sheet_from_perspective(
        Player::P1,
        &[p1_lead],
        &[zoroark, dragonite, corviknight, milotic],
        pd,
        1,
        4,
        50,
        crate::information::unknowns::InformationMode::OpenTeamSheet,
        true,
    );
    let preview: crate::information::unknowns::UnknownTeamPreviewState = match preview_state {
        crate::information::unknowns::UnknownMatchState::TeamPreview(tp) => tp,
        _ => panic!("expected TeamPreview state"),
    };
    let mut state = preview.into_battle_state(Player::P1, &[0], &[], &[1, 2, 3], &[0]);
    assert_eq!(state.p2_unresolved_zoroark_count, 1);

    assert_all_p2_items_known(&state, "team preview");

    // P2 leads Zoroark, disguised as Dragonite. Never revealed for the rest of
    // this test — exercising the un-revealed switch-out path, not the promotion
    // path already covered above.
    state = apply_real_dex(
        state,
        vec![event(EventKind::SimultaneousSwitch {
            switches: vec![
                SwitchState { disguise_species: None, max_hp: 0,
                    slot: p1(0), species: Species::Snorlax, level: 50,
                    hp: PokemonHP::Number(200), status: None, tera_type: None,
                },
                SwitchState { disguise_species: None, max_hp: 0,
                    slot: p2(0), species: Species::Dragonite, level: 50,
                    hp: PokemonHP::Percent(100), status: None, tera_type: None,
                },
            ],
        })],
        pd, md,
    );
    assert_all_p2_items_known(&state, "after disguised lead");
    assert!(
        state.p2_active_mons[0].possible_illusion_state.is_some(),
        "the disguised lead must carry a live Zoroark hypothesis"
    );

    // Corviknight switches in — the disguised "Dragonite" benches, unrevealed.
    state = apply_real_dex(state, vec![switch_in(Species::Corviknight, p2(0))], pd, md);
    assert_all_p2_items_known(&state, "after Corviknight switches in");
    let benched_dragonite = combined_back(&state, &Player::P2)
        .into_iter()
        .find(|m| matches!(&m.possible_species, Unknown::Known(Species::Dragonite)))
        .expect("the un-revealed disguised mon must bench under its shown identity");
    assert!(
        benched_dragonite.possible_illusion_state.is_some(),
        "the benched, still-unrevealed disguise must keep its Zoroark hypothesis"
    );
    assert!(
        state.p2_active_mons[0].possible_illusion_state.is_some(),
        "Corviknight itself must also carry its own hypothesis — the side's \
         Zoroark is still unresolved and could physically be either mon"
    );

    // Milotic switches in — Corviknight benches with ITS own hypothesis intact.
    state = apply_real_dex(state, vec![switch_in(Species::Milotic, p2(0))], pd, md);
    assert_all_p2_items_known(&state, "after Milotic switches in");
    let benched_corviknight = combined_back(&state, &Player::P2)
        .into_iter()
        .find(|m| matches!(&m.possible_species, Unknown::Known(Species::Corviknight)))
        .expect("Corviknight must bench under its own identity");
    assert!(benched_corviknight.possible_illusion_state.is_some());

    // The disguised mon ("Dragonite") switches back in — must re-match its OWN
    // benched entry (open-sheet set + hypothesis intact), never rebuilt from
    // scratch by any fallback path.
    state = apply_real_dex(state, vec![switch_in(Species::Dragonite, p2(0))], pd, md);
    assert_all_p2_items_known(&state, "after Dragonite returns");
    let active = &state.p2_active_mons[0];
    assert_eq!(active.possible_species, Unknown::Known(Species::Dragonite));
    assert_eq!(active.item, Unknown::Known(Item::HeavyDutyBoots));
    assert!(
        active.possible_illusion_state.is_some(),
        "the returning disguise must resume its still-live, never-broken hypothesis"
    );
}

/// F4 (TODO.md: "Same thing is true if zoroark's partner switches out"): in
/// doubles, a disguised Zoroark's PARTNER switching out (an unrelated slot on
/// the same side, not the disguised slot itself) must not disturb the disguised
/// slot's hypothesis, and the mon that replaces the partner must itself receive/
/// retain its own hypothesis — the side's Zoroark is still unresolved and, from
/// the observer's perspective, could physically be any not-yet-ruled-out mon on
/// that side, not just the one that happened to switch. Also exercises the
/// doubles-specific `mon_idx` bookkeeping (both active segments fixed at the
/// front — see the module README's "S1" note) to make sure slot0/slot1 never
/// cross-contaminate.
#[test]
fn test_zoroark_doubles_partner_switch_out_preserves_hypotheses() {
    use crate::state::pokemon::{build_pokemon_state, Nature};
    use crate::tests::simuilator_test_helpers::{move_dex, pokemon_dex};

    let pd = pokemon_dex();
    let md = move_dex();

    let p1a = build_pokemon_state(
        Species::Snorlax, pd, md, Some(50), None, None, None, None, None, None, None, None, true,
    );
    let p1b = build_pokemon_state(
        Species::Garchomp, pd, md, Some(50), None, None, None, None, None, None, None, None, true,
    );
    let zoroark = build_pokemon_state(
        Species::Zoroark, pd, md, Some(50),
        Some([Some(PokemonMove::NastyPlot), Some(PokemonMove::DarkPulse), None, None]),
        None, Some(Ability::Illusion), Some(Nature::Timid), Some(Item::ChoiceSpecs),
        None, None, None, true,
    );
    let dragonite = build_pokemon_state(
        Species::Dragonite, pd, md, Some(50),
        Some([Some(PokemonMove::DragonDance), Some(PokemonMove::ExtremeSpeed), None, None]),
        None, Some(Ability::Multiscale), Some(Nature::Adamant), Some(Item::HeavyDutyBoots),
        None, None, None, true,
    );
    let corviknight = build_pokemon_state(
        Species::Corviknight, pd, md, Some(50),
        Some([Some(PokemonMove::BraveBird), Some(PokemonMove::Roost), None, None]),
        None, Some(Ability::Pressure), Some(Nature::Impish), Some(Item::Leftovers),
        None, None, None, true,
    );
    let milotic = build_pokemon_state(
        Species::Milotic, pd, md, Some(50),
        Some([Some(PokemonMove::Scald), Some(PokemonMove::Recover), None, None]),
        None, Some(Ability::MarvelScale), Some(Nature::Calm), Some(Item::AssaultVest),
        None, None, None, true,
    );

    let preview_state = crate::information::unknowns::UnknownMatchState::team_preview_open_sheet_from_perspective(
        Player::P1,
        &[p1a, p1b],
        &[zoroark, dragonite, corviknight, milotic],
        pd,
        2,
        4,
        50,
        crate::information::unknowns::InformationMode::OpenTeamSheet,
        true,
    );
    let preview: crate::information::unknowns::UnknownTeamPreviewState = match preview_state {
        crate::information::unknowns::UnknownMatchState::TeamPreview(tp) => tp,
        _ => panic!("expected TeamPreview state"),
    };
    // P2 params are ignored for viewer=P1 (its whole roster dumps to possible_back
    // regardless — see `into_battle_state`'s doc comment), so these are placeholders.
    let mut state = preview.into_battle_state(Player::P1, &[0, 1], &[], &[1, 2], &[0, 3]);
    assert_eq!(state.p2_unresolved_zoroark_count, 1);

    // P2 leads slot0 = Zoroark disguised as Dragonite, slot1 = the genuine Corviknight.
    state = apply_real_dex(
        state,
        vec![event(EventKind::SimultaneousSwitch {
            switches: vec![
                SwitchState { disguise_species: None, max_hp: 0,
                    slot: p1(0), species: Species::Snorlax, level: 50,
                    hp: PokemonHP::Number(200), status: None, tera_type: None,
                },
                SwitchState { disguise_species: None, max_hp: 0,
                    slot: p1(1), species: Species::Garchomp, level: 50,
                    hp: PokemonHP::Number(200), status: None, tera_type: None,
                },
                SwitchState { disguise_species: None, max_hp: 0,
                    slot: p2(0), species: Species::Dragonite, level: 50,
                    hp: PokemonHP::Percent(100), status: None, tera_type: None,
                },
                SwitchState { disguise_species: None, max_hp: 0,
                    slot: p2(1), species: Species::Corviknight, level: 50,
                    hp: PokemonHP::Percent(100), status: None, tera_type: None,
                },
            ],
        })],
        pd, md,
    );
    assert_all_p2_items_known(&state, "doubles lead");
    assert!(
        state.p2_active_mons[0].possible_illusion_state.is_some(),
        "the disguised slot0 must carry a Zoroark hypothesis"
    );
    assert!(
        state.p2_active_mons[1].possible_illusion_state.is_some(),
        "slot1 (Corviknight) must ALSO independently carry a hypothesis — the \
         side's Zoroark is still unresolved and isn't pinned to slot0 specifically"
    );

    // The PARTNER (slot1) switches out — Milotic switches in. Slot0's disguise is
    // never touched or revealed at any point in this test.
    state = apply_real_dex(state, vec![switch_in(Species::Milotic, p2(1))], pd, md);
    assert_all_p2_items_known(&state, "after partner switch-out");

    assert_eq!(
        state.p2_active_mons[0].possible_species,
        Unknown::Known(Species::Dragonite),
        "the disguised slot0 must be completely undisturbed by its partner switching"
    );
    assert!(
        state.p2_active_mons[0].possible_illusion_state.is_some(),
        "slot0's hypothesis must survive its partner's switch-out"
    );
    assert!(
        state.p2_active_mons[1].possible_illusion_state.is_some(),
        "Milotic, replacing the partner, must receive/retain its own hypothesis"
    );
    let benched_corviknight = combined_back(&state, &Player::P2)
        .into_iter()
        .find(|m| matches!(&m.possible_species, Unknown::Known(Species::Corviknight)))
        .expect("the switched-out partner must bench under its own identity");
    assert!(
        benched_corviknight.possible_illusion_state.is_some(),
        "the benched former partner must keep its own hypothesis too"
    );

    // The former partner (Corviknight) returns to slot1 — must re-match its own
    // benched entry (open-sheet set + hypothesis intact), and slot0 must still be
    // completely unaffected.
    state = apply_real_dex(state, vec![switch_in(Species::Corviknight, p2(1))], pd, md);
    assert_all_p2_items_known(&state, "after partner returns");
    assert_eq!(state.p2_active_mons[1].possible_species, Unknown::Known(Species::Corviknight));
    assert_eq!(state.p2_active_mons[1].item, Unknown::Known(Item::Leftovers));
    assert!(state.p2_active_mons[1].possible_illusion_state.is_some());
    assert_eq!(
        state.p2_active_mons[0].possible_species,
        Unknown::Known(Species::Dragonite),
        "slot0 must remain untouched throughout"
    );
    assert!(state.p2_active_mons[0].possible_illusion_state.is_some());
}

fn combined_back<'a>(state: &'a UnknownBattleState, player: &Player) -> Vec<&'a UnknownPokemonState> {
    match player {
        Player::P1 => state.p1_known_back_mons.iter().chain(state.p1_possible_back_mons.iter()).collect(),
        Player::P2 => state.p2_known_back_mons.iter().chain(state.p2_possible_back_mons.iter()).collect(),
    }
}

/// Every P2 roster member's (active + bench) open-sheet item must be `Known` at
/// this checkpoint — the direct encoding of the TODO.md complaint ("it displays
/// that we have no information about that mon, but it should use the teamsheet
/// information"). Used by the open-sheet Zoroark switching regression tests to
/// assert no roster member EVER regresses to "no information", at any point in
/// an arbitrarily long switch sequence.
fn assert_all_p2_items_known(state: &UnknownBattleState, checkpoint: &str) {
    let mut all: Vec<&UnknownPokemonState> = state.p2_active_mons.iter().collect();
    all.extend(combined_back(state, &Player::P2));
    for mon in all {
        assert!(
            matches!(mon.item, Unknown::Known(_)),
            "[{checkpoint}] open-sheet item regressed to non-Known for {:?}: {:?}",
            mon.possible_species, mon.item
        );
    }
}

fn combined_back_species(state: &UnknownBattleState, player: &Player) -> Vec<Species> {
    let (known, possible) = match player {
        Player::P1 => (&state.p1_known_back_mons, &state.p1_possible_back_mons),
        Player::P2 => (&state.p2_known_back_mons, &state.p2_possible_back_mons),
    };
    known
        .iter()
        .chain(possible.iter())
        .filter_map(|m| match &m.possible_species {
            Unknown::Known(s) => Some(s.clone()),
            _ => None,
        })
        .collect()
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
            vec![event(EventKind::DamageDealt { max_hp: 0, target: p2(0), new_hp: PokemonHP::Percent(80) })],
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

/// TODO.md: "Errors with the inference engine should also add the Event currently
/// being resolved as text." `inference_contradiction!` panics must now name the event
/// being processed (`event={:?}`) alongside the pre-existing `context=` field — not
/// just a bare mon index, which was the entire complaint (`context=0` gave no clue
/// what actually went wrong). The event breadcrumb comes from a thread-local set by
/// `process_battle_event` per node, so a per-event-walk contradiction (like this one)
/// must show the specific `StatusInflicted` node, not a generic placeholder.
#[test]
#[should_panic(expected = "event=StatusInflicted")]
fn test_contradiction_panic_names_the_resolving_event() {
    let mut mon = unknown_mon();
    mon.status = Some(Status::Burn);
    let state = battle_with_p2(vec![mon]);
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

/// `garchomp_dex()` plus a synthetic Zoroark entry (base Atk=60, Dark-type — no STAB
/// on a Ground-type move, no shared typing with Garchomp) for Increment 2's
/// hypothesis-mirroring tests. Deliberately low/round stats so the resulting BSV
/// windows and damage math are hand-derivable, exactly like `garchomp_dex()`'s own
/// Atk=130 was chosen for the same reason.
fn garchomp_zoroark_dex() -> HashMap<Species, PokemonData> {
    use crate::state::pokemon::PokemonGender;
    let mut dex = garchomp_dex();
    dex.insert(Species::Zoroark, PokemonData {
        species:       Species::Zoroark,
        types:         vec![PokemonType::Dark],
        base_stats:    [60, 60, 60, 60, 60, 60], // HP Atk Def SpA SpD Spe
        weight:        811, // 81.1 kg
        primary_ability: Some(Ability::Illusion),
        abilities:     vec![Ability::Illusion],
        base_species:  None,
        forme:         None,
        required_item: None,
        battle_only:   None,
        default_gender: PokemonGender::Male,
    });
    dex
}

/// Zoroark hypothesis with *neutral* nature only, no item, no damage-boosting
/// ability — the same simplification `neutral_no_item_garchomp` uses, so the
/// resulting BSV bound is hand-computable. Attach to a host mon's
/// `possible_illusion_state` via `seed_zoroark_hypothesis_on` or directly.
fn neutral_no_item_zoroark() -> UnknownPokemonState {
    use crate::state::pokemon::Nature;
    let mut mon = UnknownPokemonState::from_opponent_species(Species::Zoroark, &garchomp_zoroark_dex(), 50);
    mon.possible_natures   = Unknown::Known(Nature::Hardy); // neutral
    mon.item               = Unknown::Known(Item::None);
    mon.possible_abilities = Unknown::Known(Ability::Illusion); // not damage-boosting
    mon.hp = PokemonHP::Percent(100);
    mon
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
    mon.min_stats         = [500, 35, 100, 55, 125, 55];
    mon.max_stats         = [500, 35, 100, 55, 125, 55];
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
            vec![event(EventKind::DamageDealt { max_hp: 0, target: p1(0), new_hp: PokemonHP::Number(new_hp) })],
        )],
        garchomp_dex(),
        move_dex,
    )
}

// ── Increment 2: Pass 3 / Pass 5 hypothesis mirroring ────────────────────────────
//
// Uses `garchomp_zoroark_dex()` (base Atk 60, Dark-type -- no STAB on Earthquake,
// no shared typing with Garchomp) so the resulting hypothesis BSV windows are
// hand-derivable exactly like the existing neutral-locked Garchomp tests above.
// Zoroark(Atk=60)'s pre-nature BSV range at level 50 is [65, 112] (same formula as
// Garchomp's own [135,182], computed at base=60 instead of base=130); at neutral
// nature, no item, no boosting ability (`neutral_no_item_zoroark`), its absolute
// MAXIMUM possible Earthquake damage (no STAB, since it's Dark- not Ground-type)
// is base(112)=floor(0.44*112)+2=51, roll=100% -> 51. Its absolute MINIMUM is
// base(65)=30, roll=85% -> 25.

fn run_direction_b_with_zoroark_hypothesis(
    damage: u16,
) -> crate::information::unknowns::UnknownBattleState {
    let mut garchomp_shown = neutral_no_item_garchomp();
    seed_zoroark_hypothesis_on(&mut garchomp_shown, &neutral_no_item_zoroark());

    let our_mon = known_p1_normal();
    let pre_hp = match our_mon.hp { PokemonHP::Number(n) => n, _ => panic!("expected Number") };
    let new_hp = pre_hp.saturating_sub(damage);
    let state = battle_1v1(our_mon, garchomp_shown);

    let mut move_dex = HashMap::new();
    move_dex.insert(PokemonMove::Earthquake, ground_physical_move(PokemonMove::Earthquake, 100));

    apply_ex(
        state,
        vec![event_with(
            EventKind::MoveUsed { user: p2(0), move_used: PokemonMove::Earthquake, targets: vec![p1(0)] },
            vec![event(EventKind::DamageDealt { max_hp: 0, target: p1(0), new_hp: PokemonHP::Number(new_hp) })],
        )],
        garchomp_zoroark_dex(),
        move_dex,
    )
}

/// damage=45 is unreachable at Zoroark's seeded minimum BSV=65 (max possible output
/// there is 30, no STAB) but comfortably reachable at its maximum BSV=112 (range
/// [43,51], rolls 89/90 give exactly 45) -- the hypothesis's min bound must rise
/// strictly above 65, while the hypothesis itself survives (45 is within its
/// achievable range at the high end).
#[test]
fn test_zoroark_pass3_direction_b_tightens_hypothesis() {
    let result = run_direction_b_with_zoroark_hypothesis(45);
    let hyp = result.p2_active_mons[0].possible_illusion_state.as_ref().expect(
        "damage=45 is within Zoroark's achievable range at its high-BSV end -- \
         the hypothesis must survive",
    );
    assert!(
        hyp.min_pre_nature_stat[1] > 65,
        "damage=45 is unreachable at Zoroark's seeded min BSV=65 (max output there \
         is 30) -- the hypothesis's own min bound must have risen; got {}",
        hyp.min_pre_nature_stat[1]
    );

    // Pass 5 synergy: EVs/IVs (or nature) back-solved from the now-tightened
    // pre-nature stat window must also reflect SOME narrowing relative to the
    // fully-uninformed default (`from_opponent_species`'s full [0,252] EV range),
    // proving `run_pass5_all_mons`'s mirrored call actually engaged for this
    // hypothesis too, not just Pass 3's direct field write.
    assert!(
        hyp.min_evs[1] > 0 || hyp.max_evs[1] < 252,
        "Pass 5's mirrored back-solve must narrow the hypothesis's own Atk EV range \
         from the tightened stat window; got [{}, {}]",
        hyp.min_evs[1], hyp.max_evs[1]
    );
}

/// damage=25 narrows Zoroark's search to the exact single point BSV=65 (its own
/// seeded minimum, only reachable at roll=85%) -- a valid BSV-level window on its
/// own, but Pass 5's EV/IV back-solve (empirically verified) finds NO valid
/// EV/IV/nature combination lands exactly on that single point, panicking on the
/// hypothesis specifically. Its own mirrored `apply_with_illusion_mirroring` call
/// (Increment 2) catches this and drops the hypothesis -- proving the Pass3->Pass5
/// synergy's OTHER direction (dropping a hypothesis, not just promoting one) also
/// works. The primary (Garchomp, whose own window at this damage is unaffected --
/// 25 is far below its own achievable range) is untouched.
#[test]
fn test_zoroark_pass3_direction_b_drops_hypothesis_when_infeasible() {
    let result = run_direction_b_with_zoroark_hypothesis(25);
    assert!(
        result.p2_active_mons[0].possible_illusion_state.is_none(),
        "damage=25 narrows Zoroark's own window to an EV/IV-lattice-infeasible \
         single point -- the hypothesis must be dropped"
    );
    assert_eq!(
        result.p2_active_mons[0].possible_species,
        Unknown::Known(Species::Garchomp),
        "primary identity must be unaffected by the hypothesis's own rejection"
    );
}

/// damage=76 -- Garchomp's own absolute minimum possible Earthquake damage (base=61
/// at BSV=135, roll=85%, with STAB) -- narrows the PRIMARY's own window to an
/// EV/IV-lattice-infeasible point (empirically verified: Pass 5 panics "every
/// candidate nature is infeasible" for the primary specifically), while being
/// comfortably above Zoroark(Atk=60)'s own absolute max output (51) -- the
/// hypothesis's search finds no combination at all for this damage (a pure
/// "no new evidence" case) and survives completely untouched. `apply_with_illusion_
/// mirroring`'s promotion path (primary contradicts, hypothesis doesn't) then fires:
/// this is the Pass3->Pass5 synergy the plan describes -- Pass 3 itself never
/// panics, so Pass 5's mirrored call is the only place this primary contradiction
/// is ever discovered, and discovering it is exactly what triggers promotion.
#[test]
fn test_zoroark_pass3_pass5_promotion_synergy() {
    let result = run_direction_b_with_zoroark_hypothesis(76);

    assert_eq!(
        result.p2_active_mons[0].possible_species,
        Unknown::Known(Species::Zoroark),
        "damage=76 makes Garchomp's own stat window EV/IV-infeasible while leaving \
         Zoroark's untouched (no evidence, comfortably above its max output) -- \
         Pass 5's mirrored call must discover the primary's infeasibility and \
         promote the hypothesis; got {:?}",
        result.p2_active_mons[0].possible_species
    );
    assert!(
        result.p2_active_mons[0].possible_illusion_state.is_none(),
        "promotion clears the (now-redundant) hypothesis slot"
    );
}

/// Direction A (we attack the disguised mon): a strong special hit taking the
/// defender from 100% to 75% HP (empirically verified): narrows Garchomp's own SpD
/// window to an EV/IV-lattice-infeasible point (Pass 5 panics "every candidate
/// nature is infeasible" for the primary), while Zoroark(SpD=60)'s own search finds
/// no combination for this same hit (no evidence, untouched) -- the same
/// Pass3->Pass5 promotion synergy `test_zoroark_pass3_pass5_promotion_synergy`
/// exercises for Direction B, here for Direction A's defensive-stat mirror instead.
#[test]
fn test_zoroark_pass3_direction_a_promotes_on_defender_infeasibility() {
    let mut p1_mon = unknown_mon_species(Species::Snorlax);
    p1_mon.hp = PokemonHP::Number(500);
    p1_mon.min_stats = [500, 100, 100, 100, 100, 100];
    p1_mon.max_stats = [500, 100, 100, 100, 100, 100];
    p1_mon.item = Unknown::Known(Item::None);
    p1_mon.possible_abilities = Unknown::Known(Ability::None);

    let mut garchomp_shown = neutral_no_item_garchomp();
    garchomp_shown.hp = PokemonHP::Percent(100);
    seed_zoroark_hypothesis_on(&mut garchomp_shown, &neutral_no_item_zoroark());

    let state = battle_1v1(p1_mon, garchomp_shown);

    let mut psychic = normal_physical_move(PokemonMove::Psychic, 100);
    psychic.category = MoveCategory::Special;
    psychic.pokemon_type = PokemonType::Psychic;
    let mut move_dex = HashMap::new();
    move_dex.insert(PokemonMove::Psychic, psychic);

    let result = apply_ex(
        state,
        vec![event_with(
            EventKind::MoveUsed { user: p1(0), move_used: PokemonMove::Psychic, targets: vec![p2(0)] },
            vec![event(EventKind::DamageDealt { max_hp: 0, target: p2(0), new_hp: PokemonHP::Percent(75) })],
        )],
        garchomp_zoroark_dex(),
        move_dex,
    );

    assert_eq!(
        result.p2_active_mons[0].possible_species,
        Unknown::Known(Species::Zoroark),
        "this hit makes Garchomp's own SpD window EV/IV-infeasible while leaving \
         Zoroark's untouched -- Pass 5's mirrored call must discover the primary's \
         infeasibility and promote the hypothesis via Direction A's mirror; got {:?}",
        result.p2_active_mons[0].possible_species
    );
    assert!(
        result.p2_active_mons[0].possible_illusion_state.is_none(),
        "promotion clears the (now-redundant) hypothesis slot"
    );
}

/// S42 regression, Direction A: a direct-damage reveal (`IllusionEnded`, nested as
/// a reaction under this same hit's `DamageDealt`) must leave `possible_illusion_
/// state` cleared. Pass 1 processes `IllusionEnded` (promoting + `resolve_zoroark_
/// globally`) while walking this `MoveUsed` event's reactions, BEFORE Pass 3 runs
/// for the same event; Pass 3's Direction A mirror reads a PRE-MOVE snapshot of
/// the defender that still carries the (now-stale) hypothesis, and before the S42
/// fix (`write_back_pass3_hypothesis`) unconditionally wrote it straight back onto
/// the just-resolved live mon — leaving `is_illusion_suspected` stuck `true`
/// forever even though the species had already correctly flipped to Zoroark.
///
/// The damage (5% — 100% to 95%) is deliberately light: empirically verified, a
/// heavier hit (e.g. 50%) narrows the re-attached hypothesis's stat window enough
/// that Pass 5's own EV/IV-lattice check independently rejects it as infeasible —
/// coincidentally masking the Pass 3 bug by cleaning up its bad write-back for an
/// unrelated reason. A light hit is comfortably "no new evidence" for either
/// Garchomp's or Zoroark's Def (`compute_defender_stat_bounds` returns `None`,
/// `feasible` defaults to `true`), so nothing downstream rejects the erroneously
/// re-attached hypothesis — isolating the write-back bug on its own.
#[test]
fn test_s42_pass3_direction_a_does_not_reattach_hypothesis_after_illusion_ended() {
    let mut p1_mon = unknown_mon_species(Species::Snorlax);
    p1_mon.hp = PokemonHP::Number(500);
    p1_mon.min_stats = [500, 100, 100, 100, 100, 100];
    p1_mon.max_stats = [500, 100, 100, 100, 100, 100];
    p1_mon.item = Unknown::Known(Item::None);
    p1_mon.possible_abilities = Unknown::Known(Ability::None);

    let mut garchomp_shown = neutral_no_item_garchomp();
    garchomp_shown.hp = PokemonHP::Percent(100);
    seed_zoroark_hypothesis_on(&mut garchomp_shown, &neutral_no_item_zoroark());

    let state = battle_1v1(p1_mon, garchomp_shown);

    let mut move_dex = HashMap::new();
    move_dex.insert(PokemonMove::Earthquake, ground_physical_move(PokemonMove::Earthquake, 100));

    let result = apply_ex(
        state,
        vec![event_with(
            EventKind::MoveUsed { user: p1(0), move_used: PokemonMove::Earthquake, targets: vec![p2(0)] },
            vec![event_with(
                EventKind::DamageDealt { max_hp: 0, target: p2(0), new_hp: PokemonHP::Percent(95) },
                vec![event(EventKind::IllusionEnded { slot: p2(0), actual_species: Species::Zoroark })],
            )],
        )],
        garchomp_zoroark_dex(),
        move_dex,
    );

    assert_eq!(
        result.p2_active_mons[0].possible_species,
        Unknown::Known(Species::Zoroark),
        "IllusionEnded must promote the slot to the true (Zoroark) identity"
    );
    assert!(
        result.p2_active_mons[0].possible_illusion_state.is_none(),
        "S42: Pass 3's Direction A mirror must NOT re-attach the pre-move \
         hypothesis snapshot after IllusionEnded already resolved this mon \
         earlier in the same event-tree walk"
    );
    assert_eq!(
        result.p2_unresolved_zoroark_count, 0,
        "the side's Zoroark is now positively located"
    );
}

/// S42 regression, Direction B: Zoroark's own damaging move that's illegal for the
/// shown species (Garchomp) promotes the hypothesis via `MoveUsed`'s Pass 1
/// handler (move-legality contradiction) — but this SAME event also runs Pass 3
/// Direction B (the move dealt real damage to P1's Number-tracked HP), which reads
/// the ATTACKER's pre-move snapshot (still carrying the stale hypothesis) and,
/// before the S42 fix, wrote it straight back onto the just-promoted live mon.
/// Companion to the Direction A test above, covering the attacker-side write-back
/// site (`user_idx`) instead of the defender-side one (`target_idx`).
#[test]
fn test_s42_pass3_direction_b_does_not_reattach_hypothesis_after_learnset_promotion() {
    let mut garchomp_shown = neutral_no_item_garchomp();
    seed_zoroark_hypothesis_on(&mut garchomp_shown, &neutral_no_item_zoroark());

    let our_mon = known_p1_normal();
    let pre_hp = match our_mon.hp { PokemonHP::Number(n) => n, _ => panic!("expected Number") };

    let state = battle_1v1(our_mon, garchomp_shown);

    let mut dark_pulse = normal_physical_move(PokemonMove::DarkPulse, 80);
    dark_pulse.category = MoveCategory::Special;
    dark_pulse.pokemon_type = PokemonType::Dark;
    let mut move_dex = HashMap::new();
    move_dex.insert(PokemonMove::DarkPulse, dark_pulse);

    let mut learnset_dex: HashMap<Species, HashSet<PokemonMove>> = HashMap::new();
    learnset_dex.insert(Species::Garchomp, [PokemonMove::Earthquake].into_iter().collect());
    learnset_dex.insert(Species::Zoroark, [PokemonMove::DarkPulse].into_iter().collect());
    let config = InferenceConfig { learnset_dex, ev_total_cap: None, ..Default::default() };

    let result = apply_with_config(
        state,
        vec![event_with(
            EventKind::MoveUsed { user: p2(0), move_used: PokemonMove::DarkPulse, targets: vec![p1(0)] },
            vec![event(EventKind::DamageDealt {
                max_hp: 0,
                target: p1(0),
                new_hp: PokemonHP::Number(pre_hp.saturating_sub(30)),
            })],
        )],
        garchomp_zoroark_dex(),
        move_dex,
        config,
    );

    assert_eq!(
        result.p2_active_mons[0].possible_species,
        Unknown::Known(Species::Zoroark),
        "a move only Zoroark can know must promote the hypothesis to primary"
    );
    assert!(
        result.p2_active_mons[0].possible_illusion_state.is_none(),
        "S42: Pass 3's Direction B mirror must NOT re-attach the pre-move \
         hypothesis snapshot after the MoveUsed handler already promoted this \
         mon earlier in the same event"
    );
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

// ── S46: Direction B must not exact-match a lethal hit's damage ──────────────
//
// A hit that faints the target only reveals `exact_damage = min(true_damage, pre_hp)`
// — HP can't go negative, so any attacker offense strong enough to overkill by an
// arbitrary margin produces the identical (0 HP, Faint) observation. Treating this
// as an exact-match requirement (as the non-lethal case correctly does) unsoundly
// excludes every BSV whose damage roll exceeds `exact_damage`, which can be the
// TRUE value. Found via `random_doubles_battles_are_sound` (AerodactylMega, true
// nature Jolly — neutral for Atk — wrongly excluded down to nerf-Atk-only natures
// after a Dual Wingbeat/Rock Slide KO).

/// Same BSV/damage derivation as `test_pass3_dir_b_upper_bound_tightened` (damage=91,
/// only reachable by BSV≤161 as an *exact* match) — but here the target's whole HP
/// IS 91, so this hit is lethal. BSV=182 (Garchomp's true neutral maximum) deals well
/// over 91 at every roll, which would have (wrongly) excluded it under the old
/// exact-match rule. The upper bound must remain at the true ceiling (182), not drop
/// to 161 as it would for an equivalent non-lethal hit.
#[test]
fn test_s46_pass3_dir_b_lethal_hit_does_not_cap_upper_bound() {
    let mut p1_mon = known_p1_normal();
    p1_mon.hp = PokemonHP::Number(91);
    p1_mon.min_stats[0] = 91;
    p1_mon.max_stats[0] = 91;

    let state = battle_1v1(p1_mon, neutral_no_item_garchomp());
    let mut move_dex = HashMap::new();
    move_dex.insert(PokemonMove::Earthquake, ground_physical_move(PokemonMove::Earthquake, 100));

    let result = apply_ex(
        state,
        vec![event_with(
            EventKind::MoveUsed { user: p2(0), move_used: PokemonMove::Earthquake, targets: vec![p1(0)] },
            vec![event(EventKind::DamageDealt { max_hp: 0, target: p1(0), new_hp: PokemonHP::Number(0) })],
        )],
        garchomp_dex(),
        move_dex,
    );
    let p2 = &result.p2_active_mons[0];

    assert!(
        p2.max_pre_nature_stat[1] >= 182,
        "a lethal hit must not cap the upper BSV bound below the true ceiling (182) — \
         got {}. A lethal hit's exact_damage is a lower bound, not an exact value.",
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
            vec![event(EventKind::DamageDealt { max_hp: 0, target: p1(0), new_hp: PokemonHP::Number(450) })],
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
                vec![event(EventKind::DamageDealt { max_hp: 0, target: p1(0), new_hp: PokemonHP::Number(new_hp) })],
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
    // S39: `Crit` is emitted as a REACTION (child) of its `DamageDealt` node in the
    // real simulator (`with_reactions(bs, DamageDealt{...}, run_damage_reactions)` in
    // simulator/mod.rs — Crit is emitted from inside that closure), not a preceding
    // sibling. Nest it here to match the real event tree shape.
    let result = apply_ex(
        state,
        vec![event_with(
            EventKind::MoveUsed { user: p2(0), move_used: PokemonMove::Earthquake, targets: vec![p1(0)] },
            vec![event_with(
                EventKind::DamageDealt { max_hp: 0, target: p1(0), new_hp: PokemonHP::Number(359) }, // 141 damage
                vec![event(EventKind::Crit { target: p1(0) })], // crit signalled
            )],
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

/// S32 regression (TODO.md "SpeedComparison raises min above max" / RagePowder
/// VolatileEnd; also the Tailwind+Heat Wave doubles bug): `pass4_speed_from_order`
/// runs twice per `apply_information_battle` call — once before the event walk
/// (correct: `state` is still pre-turn) and once after (previously buggy: it re-read
/// Tailwind/Trick Room/weather live from `state`, which the walk had by then mutated
/// to end-of-turn field conditions).
///
/// P1 casts Tailwind on its own side and moves FIRST (priority 0); P2 moves SECOND
/// (priority 0). At the moment this pairing's ordering was actually determined —
/// just before P1's own move — Tailwind was NOT yet up, so it must not factor into
/// the numeric multiplier baked into the resulting `SpeedComparison`. The buggy
/// second-pass seed would have credited P1 with its own same-turn Tailwind
/// retroactively (fast_mult=2 instead of 1), which is unsound: it can force a fast
/// mon's minimum Spe far above what's actually observed, exactly the "raises
/// min(N) above max(M)" contradiction from the bug report (there mislabeled with a
/// stale `event=VolatileEnd`/`event=EndOfTurn` breadcrumb — Pass 4 never reads
/// those event kinds at all; see the `CURRENT_EVENT_CONTEXT` reset in
/// `apply_information_battle`).
#[test]
fn test_s32_own_turn_tailwind_not_retroactive_to_pre_cast_pairing() {
    let p1_mon = no_speed_escape_mon(Species::Garchomp);
    let p2_mon = no_speed_escape_mon(Species::Garchomp);
    let state = battle_1v1(p1_mon, p2_mon);

    let mut move_dex = HashMap::new();
    move_dex.insert(PokemonMove::Tailwind, poke_status_move(PokemonMove::Tailwind));
    move_dex.insert(PokemonMove::DragonClaw, normal_physical_move(PokemonMove::DragonClaw, 80));

    let tailwind_event = event_with(
        EventKind::MoveUsed { user: p1(0), move_used: PokemonMove::Tailwind, targets: vec![] },
        vec![event(EventKind::SideConditionStart { side: Player::P1, condition: SideCondition::TailWind })],
    );

    let result = apply_ex(
        state,
        vec![
            tailwind_event,
            event(EventKind::MoveUsed { user: p2(0), move_used: PokemonMove::DragonClaw, targets: vec![p1(0)] }),
        ],
        HashMap::new(),
        move_dex,
    );

    // Correct: fast=P1(idx0), slow=P2(idx1), multiplier (4,4) — the neutral (no
    // boost/paralysis/Tailwind) baseline from `compute_speed_multipliers`'s
    // unreduced stage fractions (stage_frac(0) = (2,2) on both sides, squared by the
    // cross-multiply). Tailwind wasn't up yet when this pairing raced, so it must
    // not appear in either factor.
    let has_correct = result.predicates.iter().any(|clause| {
        clause.iter().any(|stmt| matches!(stmt,
            Statement::SpeedComparison { fast_idx: 0, slow_idx: 1, fast_mult: 4, slow_mult: 4 }))
    });
    assert!(
        has_correct,
        "expected an un-tailwinded SpeedComparison(fast=0,slow=1,mult=4,4); predicates = {:?}",
        result.predicates
    );

    // Bug: fast_mult=8 (double the neutral 4) would mean P1's own same-turn Tailwind
    // cast was wrongly applied retroactively to the pairing that put it first.
    let has_buggy = result.predicates.iter().any(|clause| {
        clause.iter().any(|stmt| matches!(stmt,
            Statement::SpeedComparison { fast_idx: 0, slow_idx: 1, fast_mult: 8, .. }))
    });
    assert!(
        !has_buggy,
        "P1's own same-turn Tailwind cast must not retroactively speed up the pairing \
         that put it first; predicates = {:?}",
        result.predicates
    );
}

/// S32 regression, Trick Room half of the same bug: P1 sets Trick Room on its own
/// move (priority-0 stub) and moves FIRST; P2 moves SECOND. Trick Room was NOT up
/// when this pairing raced (it's the reaction of P1's own move), so the pairing
/// must be read as a NORMAL (non-reversed) ordering: fast=P1(idx0), slow=P2(idx1).
///
/// Previously, `pass4_speed_from_order`'s *second* call (after the event walk has
/// mutated `state.pseudo_weathers` to include the now-active Trick Room) read Trick
/// Room as a single live global instead of per-pairing, and would have wrongly
/// swapped this pairing to fast=P2/slow=P1 — misreading the very setup turn.
#[test]
fn test_s32_own_turn_trick_room_not_retroactive_to_pre_cast_pairing() {
    let p1_mon = no_speed_escape_mon(Species::Garchomp);
    let p2_mon = no_speed_escape_mon(Species::Garchomp);
    let state = battle_1v1(p1_mon, p2_mon);

    let mut move_dex = HashMap::new();
    move_dex.insert(PokemonMove::TrickRoom, poke_status_move(PokemonMove::TrickRoom));
    move_dex.insert(PokemonMove::DragonClaw, normal_physical_move(PokemonMove::DragonClaw, 80));

    let trick_room_event = event_with(
        EventKind::MoveUsed { user: p1(0), move_used: PokemonMove::TrickRoom, targets: vec![] },
        vec![event(EventKind::PseudoWeatherStart { effect: PseudoWeather::TrickRoom })],
    );

    let result = apply_ex(
        state,
        vec![
            trick_room_event,
            event(EventKind::MoveUsed { user: p2(0), move_used: PokemonMove::DragonClaw, targets: vec![p1(0)] }),
        ],
        HashMap::new(),
        move_dex,
    );

    let has_correct = result.predicates.iter().any(|clause| {
        clause.iter().any(|stmt| matches!(stmt, Statement::SpeedComparison { fast_idx: 0, slow_idx: 1, .. }))
    });
    assert!(
        has_correct,
        "expected the un-reversed SpeedComparison(fast=0,slow=1) since Trick Room \
         wasn't up when this pairing raced; predicates = {:?}",
        result.predicates
    );

    let has_wrongly_swapped = result.predicates.iter().any(|clause| {
        clause.iter().any(|stmt| matches!(stmt, Statement::SpeedComparison { fast_idx: 1, slow_idx: 0, .. }))
    });
    assert!(
        !has_wrongly_swapped,
        "P1's own same-turn Trick Room cast must not retroactively reverse the pairing \
         that put it first; predicates = {:?}",
        result.predicates
    );
}

/// S33 regression: `pass5_back_solve`'s HP contradiction ("no IV/EV can produce
/// observed HP bounds") must self-heal rather than panic when `min_stats[0]`/
/// `max_stats[0]` is left unreachable by the mon's current species/EV/IV window —
/// the same class of stale-bound desync S30 fixed for its one known trigger
/// (`HasSpecies` forced mid-BCP after `widen_item_for_illusion`). A code audit found
/// no OTHER call site can legitimately narrow `min_stats[0]`/`max_stats[0]` from an
/// observation (percent/damage updates only ever touch the display field `mon.hp`;
/// `Statement::EVIVStatGE/LE` can never target HP — there is no `PokemonStat::Hp`
/// variant), so as a defense-in-depth measure this directly corrupts the HP window
/// to simulate an as-yet-unaudited trigger of the same shape, and asserts recovery:
/// no panic, and the window widens back to the species' theoretical worst/best case
/// (the real achievable HP for the true species/level must lie within it).
#[test]
fn test_s33_pass5_hp_self_heals_instead_of_panicking() {
    let pd = crate::tests::simuilator_test_helpers::pokemon_dex();
    let mut mon = UnknownPokemonState::from_opponent_species(Species::Charizard, pd, 50);
    mon.possible_species = Unknown::Known(Species::Charizard);
    // Simulate a stale HP window left over from some other context — unreachable by
    // any Charizard IV/EV combination at level 50.
    mon.min_stats[0] = 9000;
    mon.max_stats[0] = 9001;
    // Also simulate a stale, too-narrow EV bound from a prior (now-invalidated) pass5
    // call, to confirm the self-heal resets it rather than leaving it stuck.
    mon.min_evs[0] = 200;
    mon.max_evs[0] = 210;

    let config = InferenceConfig::default();
    pass5_back_solve(&mut mon, &config, pd); // must not panic

    assert!(
        mon.min_stats[0] <= mon.max_stats[0],
        "healed HP window must be non-empty: [{}, {}]",
        mon.min_stats[0], mon.max_stats[0]
    );
    assert_eq!(mon.min_evs[0], 0, "healed window must widen EVs back to the full range");
    assert_eq!(mon.max_evs[0], 252, "healed window must widen EVs back to the full range");

    // Soundness: the real Charizard's achievable HP range (base HP, level 50) must lie
    // within the healed window at both IV extremes.
    let base_hp = pd.get(&Species::Charizard).unwrap().base_stats[0];
    let iv_lo = if config.force_max_ivs { 31 } else { mon.min_ivs[0] };
    let iv_hi = if config.force_max_ivs { 31 } else { mon.max_ivs[0] };
    let real_lo = crate::state::pokemon::calc_hp(base_hp, iv_lo, 0, 50);
    let real_hi = crate::state::pokemon::calc_hp(base_hp, iv_hi, 252, 50);
    assert!(
        mon.min_stats[0] <= real_lo && real_hi <= mon.max_stats[0],
        "healed window [{}, {}] must contain the true achievable HP range [{}, {}]",
        mon.min_stats[0], mon.max_stats[0], real_lo, real_hi
    );
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
                event(EventKind::DamageDealt { max_hp: 0, target: p1(0), new_hp: PokemonHP::Number(460) }),
                event(EventKind::DamageDealt { max_hp: 0, target: p1(0), new_hp: PokemonHP::Number(422) }),
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

/// **S23 regression — per-hit crit attribution.** A 2-hit move lands a crit on hit 2
/// only. S39: the sim emits `Crit` nested as a REACTION (child) of that hit's own
/// `DamageDealt`, not a preceding sibling — see `with_reactions(bs, DamageDealt{...},
/// run_damage_reactions)` in simulator/mod.rs.
///
/// Setup mirrors `test_pass3_multihit_tighter_than_single_hit` (Garchomp, neutral,
/// 40 BP Normal 2-hit vs Def=100): true Atk BSV = 180 → base term 33, non-crit rolls
/// 28–33, crit rolls 42–49. Observed: hit 1 NON-crit for 33, hit 2 crit for 45.
///
/// Before the S23 fix, one `Crit` reaction set a global flag for EVERY hit, so hit 1
/// was constrained to Atk values whose CRIT rolls produce 33 — Atk ∈ ≈[135, 147],
/// entirely below the true 180 — capping `max_pre_nature_stat[1]` at ≈147 before the
/// genuinely-crit hit 2 (feasible only at Atk ≥ 159) even got scanned.
#[test]
fn test_s23_multihit_mixed_crit_keeps_true_atk_feasible() {
    let p1_mon = known_p1_normal(); // HP=500, Def=100
    let p2_mon = neutral_no_item_garchomp();

    let mut move_dex = HashMap::new();
    let mut two_hit = normal_physical_move(PokemonMove::BulletSeed, 40);
    two_hit.multihit_range = [2, 2];
    move_dex.insert(PokemonMove::BulletSeed, two_hit);

    let state = battle_1v1(p1_mon, p2_mon);
    let result = apply_ex(
        state,
        vec![event_with(
            EventKind::MoveUsed { user: p2(0), move_used: PokemonMove::BulletSeed, targets: vec![p1(0)] },
            vec![
                // Hit 1: non-crit, 33 damage (500 → 467), no nested Crit.
                event(EventKind::DamageDealt { max_hp: 0, target: p1(0), new_hp: PokemonHP::Number(467) }),
                // Hit 2: crit, 45 damage (467 → 422) — Crit nested under this hit's own DamageDealt.
                event_with(
                    EventKind::DamageDealt { max_hp: 0, target: p1(0), new_hp: PokemonHP::Number(422) },
                    vec![event(EventKind::Crit { target: p1(0) })],
                ),
                event(EventKind::HitCount { target: p1(0), hits: 2 }),
            ],
        )],
        garchomp_dex(),
        move_dex,
    );

    let p2_r = &result.p2_active_mons[0];
    assert!(
        p2_r.min_pre_nature_stat[1] <= 180 && 180 <= p2_r.max_pre_nature_stat[1],
        "true Atk BSV 180 must stay within [{}, {}] — a global crit flag applied to \
         the non-crit hit excludes it",
        p2_r.min_pre_nature_stat[1],
        p2_r.max_pre_nature_stat[1]
    );
}

/// **S23 regression — mid-sequence heal moves the HP baseline.** A pinch berry (or
/// any heal) firing between two hits is emitted as its own `Healed` reaction; the
/// next hit's damage must be measured from the POST-heal HP.
///
/// True Atk BSV = 180 (base term 33): hit 1 deals 28 (500 → 472), a heal restores to
/// 478, hit 2 deals 33 (478 → 445). Before the S23 fix, the walk collected only
/// `DamageDealt` events, so hit 2's baseline stayed at 472 and its damage was
/// misread as 27 — feasible only for Atk ≤ ≈164, capping the bound below the
/// true 180.
#[test]
fn test_s23_multihit_heal_between_hits_keeps_true_atk_feasible() {
    let p1_mon = known_p1_normal(); // HP=500, Def=100
    let p2_mon = neutral_no_item_garchomp();

    let mut move_dex = HashMap::new();
    let mut two_hit = normal_physical_move(PokemonMove::BulletSeed, 40);
    two_hit.multihit_range = [2, 2];
    move_dex.insert(PokemonMove::BulletSeed, two_hit);

    let state = battle_1v1(p1_mon, p2_mon);
    let result = apply_ex(
        state,
        vec![event_with(
            EventKind::MoveUsed { user: p2(0), move_used: PokemonMove::BulletSeed, targets: vec![p1(0)] },
            vec![
                // Hit 1: 28 damage (500 → 472).
                event(EventKind::DamageDealt { max_hp: 0, target: p1(0), new_hp: PokemonHP::Number(472) }),
                // Berry-style heal on the target: 472 → 478.
                event(EventKind::Healed { max_hp: 0, target: p1(0), new_hp: PokemonHP::Number(478) }),
                // Hit 2: 33 damage measured from the healed baseline (478 → 445).
                event(EventKind::DamageDealt { max_hp: 0, target: p1(0), new_hp: PokemonHP::Number(445) }),
                event(EventKind::HitCount { target: p1(0), hits: 2 }),
            ],
        )],
        garchomp_dex(),
        move_dex,
    );

    let p2_r = &result.p2_active_mons[0];
    assert!(
        p2_r.min_pre_nature_stat[1] <= p2_r.max_pre_nature_stat[1],
        "heal-blind baseline must not invert Atk bounds (min {}, max {})",
        p2_r.min_pre_nature_stat[1],
        p2_r.max_pre_nature_stat[1]
    );
    assert!(
        p2_r.min_pre_nature_stat[1] <= 180 && 180 <= p2_r.max_pre_nature_stat[1],
        "true Atk BSV 180 must stay within [{}, {}] — a heal-blind baseline misreads \
         hit 2's damage as 27 and excludes it",
        p2_r.min_pre_nature_stat[1],
        p2_r.max_pre_nature_stat[1]
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
                event(EventKind::DamageDealt { max_hp: 0, target: p1(0), new_hp: PokemonHP::Number(472) }), // 28 dmg
                event(EventKind::DamageDealt { max_hp: 0, target: p1(0), new_hp: PokemonHP::Number(443) }), // 29 dmg
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
            vec![event(EventKind::DamageDealt { max_hp: 0, target: p1(0), new_hp: PokemonHP::Number(472) })], // 28 dmg
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
/// So max_evs for SpA/SpD/Spe should each be ≤ 196 after the cap is applied.
#[test]
fn test_ev_cap_tightens_remaining_stats() {
    use crate::state::pokemon::Nature;

    let mut mon = UnknownPokemonState::from_opponent_species(Species::Garchomp, &garchomp_dex(), 50);
    mon.possible_natures = Unknown::Known(Nature::Hardy); // neutral

    // Pin Atk (stat 1) to exactly 175: requires EV≈196 (31 IVs, neutral, lv50 Garchomp base=130).
    // calc_stat(130, 31, 196, 50, 1.0) = floor((260+31+49)*0.5+5) = floor(175) = 175.
    mon.min_stats[1] = 175;
    mon.max_stats[1] = 175;
    mon.min_pre_nature_stat[1] = 175;
    mon.max_pre_nature_stat[1] = 175;

    // Pin Def (stat 2) to exactly 130: requires EV≈116 (31 IVs, neutral, lv50 Garchomp base=95).
    // calc_stat(95, 31, 116, 50, 1.0) = floor((190+31+29)*0.5+5) = floor(130) = 130.
    mon.min_stats[2] = 130;
    mon.max_stats[2] = 130;
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
    assert_eq!(mon.min_evs[1], 196, "Atk minEV must be 196 for stat=175");
    assert_eq!(mon.max_evs[1], 196, "Atk maxEV must be 196 for stat=175");
    assert!(mon.min_evs[2] <= 116, "Def minEV must be ≤ 116 for stat≥130");

    // After cap (budget = 510 - 196 - 116 - 0..= 198), other stats must have maxEV ≤ 196.
    // (Nearest EV_LATTICE value ≤ 198 is 196.)
    assert!(
        mon.max_evs[3] <= 196,
        "SpA maxEV must be capped to ≤ 196 by the total-EV cap (got {})",
        mon.max_evs[3]
    );
    assert!(
        mon.max_evs[4] <= 196,
        "SpD maxEV must be capped to ≤ 196 by the total-EV cap (got {})",
        mon.max_evs[4]
    );
    assert!(
        mon.max_evs[5] <= 196,
        "Spe maxEV must be capped to ≤ 196 by the total-EV cap (got {})",
        mon.max_evs[5]
    );

    // Soundness check: min ≤ max for all stats.
    for i in 0..6 {
        assert!(mon.min_evs[i] <= mon.max_evs[i], "EV bounds inverted for stat {}", i);
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

    // When min_evs are all 0, budget per stat = 510 - 0 = 510 ≥ 252 → no cap tightening.
    for i in 0..6 {
        assert!(mon.max_evs[i] <= 252, "maxEV must never exceed 252 (got {} for stat {})", mon.max_evs[i], i);
        assert!(mon.min_evs[i] <= mon.max_evs[i], "EV bounds inverted for stat {}", i);
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
            vec![event(EventKind::DamageDealt { max_hp: 0, target: p2(0), new_hp: PokemonHP::Percent(50) })],
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

// ── Regression: S21 — item suppression silences item reactions ──────────────────

/// Under Magic Room (`MagicDeluge`), a genuinely-held Rocky Helmet produces no chip
/// (the sim gates it on `item_is_active`), so the missing reaction is not evidence of
/// absence. Before the S21 fix, the contact-absence pass excluded Rocky Helmet on the
/// defender anyway — excluding the true held item.
#[test]
fn test_s21_no_helmet_exclusion_under_magic_room() {
    let p1_mon = {
        let mut m = UnknownPokemonState::from_opponent_species(Species::Garchomp, &garchomp_dex(), 50);
        m.item = Unknown::Known(Item::None);
        m.possible_abilities = Unknown::Known(Ability::SandVeil);
        m
    };
    let p2_mon = UnknownPokemonState::from_opponent_species(Species::Garchomp, &garchomp_dex(), 50);
    let mut state = battle_1v1(p1_mon, p2_mon);
    state.pseudo_weathers = vec![PseudoWeather::MagicDeluge];
    state.pseudo_weather_turns = vec![Unknown::Known(3)];

    let mut move_dex = HashMap::new();
    move_dex.insert(PokemonMove::Earthquake, contact_physical_move(PokemonMove::Earthquake, 100));

    let result = apply_ex(
        state,
        vec![event_with(
            EventKind::MoveUsed { user: p1(0), move_used: PokemonMove::Earthquake, targets: vec![p2(0)] },
            vec![event(EventKind::DamageDealt { max_hp: 0, target: p2(0), new_hp: PokemonHP::Percent(50) })],
        )],
        garchomp_dex(),
        move_dex,
    );

    assert!(
        !unknown_is_excluded(&result.p2_active_mons[0].item, &Item::RockyHelmet),
        "Rocky Helmet must not be excluded while Magic Room suppresses items"
    );
}

/// Life Orb analogue: the LO chip is gated on `item_is_active` in the sim, so
/// missing recoil under Magic Room says nothing about the attacker's item.
#[test]
fn test_s21_no_life_orb_exclusion_under_magic_room() {
    let p1_mon = {
        let mut m = UnknownPokemonState::from_opponent_species(Species::Garchomp, &garchomp_dex(), 50);
        m.item = Unknown::Known(Item::None);
        m.possible_abilities = Unknown::Known(Ability::SandVeil);
        m
    };
    // Attacker (P2) with the recoil-escape abilities excluded — only Magic Room
    // stands between "no recoil" and the (unsound) Life Orb exclusion.
    let p2_mon = UnknownPokemonState::from_opponent_species(Species::Garchomp, &garchomp_dex(), 50);
    let mut state = battle_1v1(p1_mon, p2_mon);
    state.pseudo_weathers = vec![PseudoWeather::MagicDeluge];
    state.pseudo_weather_turns = vec![Unknown::Known(3)];

    let mut move_dex = HashMap::new();
    move_dex.insert(PokemonMove::Earthquake, normal_physical_move(PokemonMove::Earthquake, 100));

    let result = apply_ex(
        state,
        vec![event_with(
            EventKind::MoveUsed { user: p2(0), move_used: PokemonMove::Earthquake, targets: vec![p1(0)] },
            vec![event(EventKind::DamageDealt { max_hp: 0, target: p1(0), new_hp: PokemonHP::Number(120) })],
        )],
        garchomp_dex(),
        move_dex,
    );

    assert!(
        !unknown_is_excluded(&result.p2_active_mons[0].item, &Item::LifeOrb),
        "Life Orb must not be excluded while Magic Room suppresses items"
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
            vec![event(EventKind::Healed { max_hp: 0, target: p2(0), new_hp: PokemonHP::Percent(100) })],
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
            vec![event(EventKind::Healed { max_hp: 0, target: p2(0), new_hp: PokemonHP::Percent(100) })],
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
            vec![event(EventKind::DamageDealt { max_hp: 0, target: p2(0), new_hp: PokemonHP::Percent(80) })],
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
                event(EventKind::DamageDealt { max_hp: 0, target: p2(0), new_hp: PokemonHP::Percent(0) }),
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

/// Regression (TODO.md "SpeedComparison raises min above max" / Mega Evolution): a
/// `SpeedComparison` predicate persisted from BEFORE a Mega Evolution — capping the
/// mega'd mon's max Spe against its PRE-mega base stat — must not survive the mega.
/// `recompute_stat_bounds_for_species_change` remaps `min_stats`/`max_stats` to the new
/// (here, much faster) base-stat table, and if the stale clause is left in
/// `state.predicates`, the tail BCP/Pass-4 re-run re-derives the OLD cap against the
/// freshly-raised `min_stats[5]` and contradiction-panics. The `MegaEvolution` handler
/// must purge species-derived predicates (`statement_stale_after_species_reveal`, same
/// mechanism as S30's `IllusionEnded` purge) so this never fires.
#[test]
fn test_mega_evolution_purges_stale_speed_comparison() {
    use crate::state::pokemon::PokemonGender;

    // P1: fixed, known Spe = 100 — the "fast" reference mon.
    let mut p1_mon = unknown_mon_species(Species::Garchomp);
    p1_mon.min_stats[5] = 100;
    p1_mon.max_stats[5] = 100;

    // P2: about to Mega Evolve into something MUCH faster than its current bounds allow.
    let p2_mon = unknown_mon_species(Species::Garchomp);
    let mut state = battle_1v1(p1_mon, p2_mon);
    state.p2_has_mega = true;
    // A stale, already-unit SpeedComparison clause from an earlier turn: "P1 (idx 0) is
    // at least as fast as P2 (idx 1)" — caps p2's max Spe at 100 once propagated.
    // mon_idx layout: both actives come first (`[p1_active…, p2_active…]`), so in a 1v1
    // idx 0 = P1's only active slot, idx 1 = P2's.
    state.predicates.push(vec![Statement::SpeedComparison {
        fast_idx: 0,
        slow_idx: 1,
        fast_mult: 1,
        slow_mult: 1,
    }]);

    let mut dex = HashMap::new();
    dex.insert(Species::GarchompMega, PokemonData {
        species: Species::GarchompMega,
        types: vec![PokemonType::Dragon, PokemonType::Ground],
        // Base Spe 150 (vs. Garchomp's real 102) — deliberately exaggerated so the
        // post-mega min_stats[5] floor is guaranteed to exceed the stale 100 cap.
        base_stats: [108, 170, 115, 120, 95, 150],
        weight: 950,
        primary_ability: Some(Ability::SandForce),
        abilities: vec![Ability::SandForce],
        base_species: Some(Species::Garchomp),
        forme: None,
        required_item: None,
        battle_only: Some(Species::Garchomp),
        default_gender: PokemonGender::Male,
    });

    // Does not panic — this is the primary regression assertion.
    let result = apply_ex(
        state,
        vec![event(EventKind::MegaEvolution { slot: p2(0), into: Species::GarchompMega })],
        dex,
        HashMap::new(),
    );

    let mon = &result.p2_active_mons[0];
    assert!(
        mon.min_stats[5] <= mon.max_stats[5],
        "post-mega Spe bounds must stay consistent: min={} max={}",
        mon.min_stats[5], mon.max_stats[5]
    );
    // The stale pre-mega cap must be gone — max Spe should reflect the new, much
    // faster base stat, not stay pinned at the old 100 cap.
    assert!(
        mon.max_stats[5] > 100,
        "the stale pre-mega SpeedComparison cap must be purged; max_stats[5] = {}",
        mon.max_stats[5]
    );
    assert!(
        !result.predicates.iter().any(|clause| clause.iter().any(|lit| matches!(
            lit,
            Statement::SpeedComparison { slow_idx: 1, .. } | Statement::SpeedComparison { fast_idx: 1, .. }
        ))),
        "no SpeedComparison clause referencing the mega'd mon should survive; predicates = {:?}",
        result.predicates
    );
}

/// **S41 regression** — a mon that Mega Evolves on the SAME turn it moves must use its
/// POST-mega Speed for that turn's own move-order comparisons, not its turn-start
/// (pre-mega) Speed. `pass4_speed_from_order`'s FIRST call runs BEFORE the event walk
/// (deliberately — see its doc comment, to seed Spe bounds ahead of Pass 3's damage
/// oracle) and reads `state` as of turn start for EVERY `MoveUsed` in the event list,
/// including one whose user Mega Evolves earlier in that SAME event list. Real games
/// resolve Mega Evolution before any move that turn, so by the time the mega'd mon's
/// own `MoveUsed` node is scanned, its true Speed is already the POST-mega value — but
/// pass4's first call, having not yet walked the `MegaEvolution` event, still reads the
/// PRE-mega value from `state`. If a same-priority pairing that turn is only explained
/// by the POST-mega Speed (a real, sound observation), this stale-vs-live desync can
/// produce a `SpeedComparison` this pass can't yet satisfy against pre-mega bounds.
///
/// Setup: P1's own Tyranitar (idx 1) has pre-mega Spe well BELOW P1's own second mon's
/// Spe (idx 0), and post-mega Spe well ABOVE it — so the observed order (P2 moves,
/// then Tyranitar, then the idx-0 mon) is only sound post-mega. Both P1 mons are fully
/// Known (own team), matching the real report exactly (both ends of the panicking
/// `SpeedComparison` were the observer's own, already-pinned mons).
#[test]
fn test_s41_mega_evolution_uses_post_mega_speed_for_same_turn_move_order() {
    use crate::state::pokemon::{Nature, PokemonGender};

    // P1 idx 0: fully known, fixed Spe = 150 — between the pre- and post-mega values below.
    let mut p1_other = unknown_mon_species(Species::Snorlax); // not in dex; pass5 skips it
    p1_other.min_stats[5] = 150;
    p1_other.max_stats[5] = 150;
    p1_other.item = Unknown::Known(Item::None);
    p1_other.possible_abilities = Unknown::Known(Ability::None);

    // P1 idx 1: Tyranitar, fully known/pinned (own mon) — pre-mega base Spe 80 (chosen
    // low), post-mega base Spe 120 (chosen high), same EV/IV/nature/level either side.
    let mut tyranitar = unknown_mon_species(Species::Tyranitar);
    tyranitar.possible_natures = Unknown::Known(Nature::Hardy); // neutral on every stat
    tyranitar.min_ivs = [31; 6];
    tyranitar.max_ivs = [31; 6];
    tyranitar.min_evs = [0, 0, 0, 0, 0, 252];
    tyranitar.max_evs = [0, 0, 0, 0, 0, 252];
    tyranitar.item = Unknown::Known(Item::Tyranitarite);
    tyranitar.possible_abilities = Unknown::Known(Ability::SandStream);
    tyranitar.possible_original_abilities = Unknown::Known(Ability::SandStream);
    // Pre-mega Spe (base 80, iv 31, ev 252, lvl 50, neutral): calc_stat gives 132.
    tyranitar.min_stats[5] = 132;
    tyranitar.max_stats[5] = 132;
    tyranitar.min_pre_nature_stat = [0; 6];
    tyranitar.max_pre_nature_stat = [u16::MAX; 6];

    // P2: two placeholder mons (only idx 0 acts; doubles shape matches the real report).
    let p2a = unknown_mon_species(Species::Garchomp);
    let p2b = unknown_mon_species(Species::Garchomp);

    let mut state = battle_nvn(vec![p1_other, tyranitar], vec![p2a, p2b]);
    state.p1_has_mega = true;

    let mut dex = HashMap::new();
    dex.insert(Species::Tyranitar, PokemonData {
        species: Species::Tyranitar,
        types: vec![PokemonType::Rock, PokemonType::Dark],
        base_stats: [100, 134, 110, 95, 100, 80], // Spe 80 (test value, not the real dex)
        weight: 2020,
        primary_ability: Some(Ability::SandStream),
        abilities: vec![Ability::SandStream, Ability::Unnerve],
        base_species: None,
        forme: None,
        required_item: None,
        battle_only: None,
        default_gender: PokemonGender::Male,
    });
    dex.insert(Species::TyranitarMega, PokemonData {
        species: Species::TyranitarMega,
        types: vec![PokemonType::Rock, PokemonType::Dark],
        base_stats: [100, 164, 150, 95, 120, 120], // Spe 120 (test value) — the real jump
        weight: 2550,
        primary_ability: Some(Ability::SandStream),
        abilities: vec![Ability::SandStream],
        base_species: Some(Species::Tyranitar),
        forme: None,
        required_item: Some("Tyranitarite".to_string()),
        battle_only: Some(Species::Tyranitar),
        default_gender: PokemonGender::Male,
    });

    let mut move_dex = HashMap::new();
    move_dex.insert(PokemonMove::Growl, {
        let mut m = normal_physical_move(PokemonMove::Growl, 0);
        m.category = MoveCategory::Status;
        m
    });

    // Real observed shape: MegaEvolution first, then P2 moves, then the mega'd mon
    // (only sound post-mega: 120-base Spe > 150? no — post-mega Spe here is 172,
    // computed below, comfortably above p1_other's 150), then p1_other.
    let events = vec![
        event(EventKind::MegaEvolution { slot: p1(1), into: Species::TyranitarMega }),
        event(EventKind::MoveUsed { user: p2(0), move_used: PokemonMove::Growl, targets: vec![p1(0)] }),
        event(EventKind::MoveUsed { user: p1(1), move_used: PokemonMove::Growl, targets: vec![p2(1)] }),
        event(EventKind::MoveUsed { user: p1(0), move_used: PokemonMove::Growl, targets: vec![p2(0)] }),
    ];

    // Must not panic — this is the primary regression assertion.
    let result = apply_ex(state, events, dex, move_dex);

    let mon = &result.p1_active_mons[1];
    assert!(
        mon.min_stats[5] <= mon.max_stats[5],
        "post-mega Spe bounds must stay consistent: min={} max={}",
        mon.min_stats[5], mon.max_stats[5]
    );
    // Post-mega Spe (base 120, iv 31, ev 252, lvl 50, neutral) = 172 — must not be
    // corrupted down toward the pre-mega 132 by a stale pre-mega SpeedComparison.
    assert_eq!(
        (mon.min_stats[5], mon.max_stats[5]),
        (172, 172),
        "own mon's post-mega Speed must stay exactly known at its real post-mega value"
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
            vec![event(EventKind::DamageDealt { max_hp: 0,
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
            vec![event(EventKind::DamageDealt { max_hp: 0, target: p1(0), new_hp: PokemonHP::Percent(80) })],
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
    // Lock every stat to 100; the oracle reads min_stats[3] for SpA.
    p1_mon.min_stats = [500, 100, 100, 100, 100, 100];
    p1_mon.max_stats = [500, 100, 100, 100, 100, 100];
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
    p2_mon.min_stats[0]            = 200;  // fix max-HP to 200 for deterministic % → HP conversion
    p2_mon.max_stats[0]            = 200;
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
            vec![event(EventKind::DamageDealt { max_hp: 0,
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
            vec![event(EventKind::DamageDealt { max_hp: 0,
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
            vec![event(EventKind::DamageDealt { max_hp: 0,
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
        if clause.iter().any(&is_stat_literal) {
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
            vec![event(EventKind::DamageDealt { max_hp: 0,
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
    p1_mon.min_stats = [500, 100, 100, 100, 100, 100];
    p1_mon.max_stats = [500, 100, 100, 100, 100, 100];
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
    p2_mon.min_stats[0]            = 200;
    p2_mon.max_stats[0]            = 200;
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
            vec![event(EventKind::DamageDealt { max_hp: 0,
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
            vec![event(EventKind::DamageDealt { max_hp: 0,
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

/// A dual-player harness (e.g. the doubles fuzz test) tracks a belief from EACH
/// side's perspective. Under a P2-viewer belief, P1 is the fogged/opponent side —
/// so a true-P1 attacker is NOT automatically "the observer's own known mon" the
/// way it is for a P1-viewer belief. Before the S43 fix, Direction A gated on the
/// literal `user_slot.player == Player::P1`, assuming P1 is always the observer;
/// under a P2-viewer belief this fires for ANY true-P1 attacker, including one
/// that is itself still fogged (e.g. P1's own ally-hit, P1_1 hitting P1_0) —
/// materializing an uncertain attacker's still-unresolved stats as if exact and
/// corrupting the defender's BSV window. After the fix, Direction A checks the
/// attacker's actual fields (species/stats/item/ability all `Known`) instead of
/// its side label, so it correctly skips here regardless of which side is
/// nominally "P1". Mirrors `test_pass3_dir_a_skipped_for_opponent_ally_hit` above
/// with the ambiguous pair moved onto P1's side instead of P2's.
#[test]
fn test_s43_pass3_direction_a_requires_attacker_fully_known() {
    // P1 mon 0: the defender — Garchomp, with a wide starting SpD BSV range.
    let mut p1_defender = UnknownPokemonState::from_opponent_species(
        Species::Garchomp, &garchomp_dex(), 50,
    );
    p1_defender.hp = PokemonHP::Percent(100);
    let orig_bsv_lo = p1_defender.min_pre_nature_stat[4];
    let orig_bsv_hi = p1_defender.max_pre_nature_stat[4];
    // P1 mon 1: the attacker — still fogged/uncertain, even though it's on the
    // "P1" side the old hardcoded gate trusted unconditionally.
    let p1_attacker = unknown_mon_species(Species::Snorlax);

    let p2_mons = vec![unknown_mon(), unknown_mon()];

    let mut state = battle_nvn(vec![p1_defender, p1_attacker], p2_mons);
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
                user: p1(1),
                move_used: PokemonMove::Psychic,
                targets: vec![p1(0)],
            },
            vec![event(EventKind::DamageDealt { max_hp: 0,
                target: p1(0),
                new_hp: PokemonHP::Percent(80), // 20% damage
            })],
        )],
        garchomp_dex(),
        move_dex,
    );

    // No EVIVStat predicate for the defender's SpD may be emitted — Direction A
    // must not have run at all when the attacker isn't fully known.
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
        "Direction A must not emit any defender predicate when the attacker isn't fully known"
    );

    let defender = &result.p1_active_mons[0];
    assert_eq!(
        defender.min_pre_nature_stat[4], orig_bsv_lo,
        "defender's min SpD BSV must be untouched when the attacker isn't fully known"
    );
    assert_eq!(
        defender.max_pre_nature_stat[4], orig_bsv_hi,
        "defender's max SpD BSV must be untouched when the attacker isn't fully known"
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
    p1_mon.min_stats = [200, 110, 65, 65, 110, 30]; // approximate Snorlax level-50 stats
    p1_mon.max_stats = [200, 110, 65, 65, 110, 30];
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
            vec![event(EventKind::DamageDealt { max_hp: 0,
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
    p1_mon.min_stats = [500, 100, 100, 300, 100, 100]; // SpA = 300
    p1_mon.max_stats = [500, 100, 100, 300, 100, 100];
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
    p2_mon.min_stats[0] = 183;
    p2_mon.max_stats[0] = 186;
    // Widen the SpD BSV search space to include 121 as the boundary.
    p2_mon.min_pre_nature_stat[4] = 50;
    p2_mon.max_pre_nature_stat[4] = 200;
    p2_mon.min_stats[4] = 50;
    p2_mon.max_stats[4] = 200;
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
            vec![event(EventKind::DamageDealt { max_hp: 0,
                target: p2(0),
                new_hp: PokemonHP::Percent(50), // 50% damage
            })],
        )],
        garchomp_dex(),
        move_dex,
    );

    let p2_result = &result.p2_active_mons[0];
    // The EV-lattice fix ensures hp=186 is sampled — the candidate that admits the
    // lowest feasible BSV. The old step_by(4) code (sampling only hp=183) would raise
    // min_pre_nature_stat[4] to ≥124, excluding that boundary value.
    //
    // Boundary recalibrated for S22: with the exact display-bucket damage band,
    // 100%→50% at max HP 186 pins the damage to exactly 93 (only raw HP 93 displays
    // as 50%, and 100% is display-exact), whose feasible-BSV floor is 122 — the old
    // ±0.5%-of-delta band also admitted damage 94, which made 121 look feasible.
    assert!(
        p2_result.min_pre_nature_stat[4] <= 122,
        "the lowest-BSV boundary at hp_cand=186 must remain feasible; \
         old step_by(4) would raise min_pre_nature_stat[4] to ≥124 (got {}).",
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
    // (just outside max_stats=170).
    mon.min_stats[1] = 150;
    mon.max_stats[1] = 170;

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

    // +Atk natures must be excluded: at BSV≥156 their stat always exceeds max_stats=170.
    for atk_nature in [Nature::Lonely, Nature::Adamant, Nature::Naughty, Nature::Brave] {
        assert!(
            unknown_is_excluded(&p2_mon.possible_natures, &atk_nature),
            "{atk_nature:?} (+Atk ×1.1) must be excluded after second pass5 uses \
             min_pre_nature_stat[Atk]=156 (BSV=156 → stat=171 > max_stats=170)"
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
    let oracle_config = DamageConfig { consider_crit: false, damage_rolls: 1, sample: false };

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

    let oracle_config = DamageConfig { consider_crit: false, damage_rolls: 1, sample: false };
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

// ── EOT item reveals: Leftovers ──────────────────────────────────────────────

/// Leftovers' EOT heal now nests its `Healed` event as a REACTION of `ItemRevealed`
/// (ItemRevealed is the parent here, unlike Frisk below where it's the child) — confirm
/// the item still collapses to Known regardless of which side of the tree it sits on,
/// and that the nested Healed doesn't interfere with the collapse.
#[test]
fn test_leftovers_eot_heal_reveals_item() {
    let mut foe = unknown_mon();
    foe.item = Unknown::Not(vec![]); // unknown item
    let state = battle_1v1(unknown_mon(), foe);

    let events = vec![event_with(
        EventKind::ItemRevealed { slot: p2(0), item: Item::Leftovers },
        vec![event(EventKind::Healed { max_hp: 0, target: p2(0), new_hp: PokemonHP::Percent(50) })],
    )];

    let result = apply(state, events);
    assert_eq!(
        result.p2_active_mons[0].item,
        Unknown::Known(Item::Leftovers),
        "the nested Leftovers ItemRevealed must still collapse the foe's item to Known"
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
            vec![event(EventKind::DamageDealt { max_hp: 0,
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
            vec![event(EventKind::DamageDealt { max_hp: 0, target: p1(0), new_hp: PokemonHP::Percent(80) })],
        ),
        event_with(
            EventKind::MoveUsed {
                user: p2(1),
                move_used: PokemonMove::Tackle,
                targets: vec![p1(0)],
            },
            vec![event(EventKind::DamageDealt { max_hp: 0, target: p1(0), new_hp: PokemonHP::Percent(60) })],
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
            vec![event(EventKind::DamageDealt { max_hp: 0, target: p1(0), new_hp: PokemonHP::Percent(70) })],
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
            vec![event(EventKind::DamageDealt { max_hp: 0, target: p2(0), new_hp: PokemonHP::Percent(70) })],
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
    let events_a = vec![event(EventKind::Switch(SwitchState { disguise_species: None, max_hp: 0,
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
    let events_b = vec![event(EventKind::Switch(SwitchState { disguise_species: None, max_hp: 0,
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
    mon_p1.min_stats[0] = p1_hp; mon_p1.max_stats[0] = p1_hp;
    mon_p1.min_stats[2] = p1_def; mon_p1.max_stats[2] = p1_def;

    // P2: Garchomp with Analytic, pre-nature Atk stat tightened to exactly 150
    // (corresponds to 31 IVs, 0 EVs, neutral nature at level 50).
    let true_atk_bsv = 150u16;
    let mut mon_p2 = unknown_mon_species(Species::Garchomp);
    mon_p2.possible_abilities = Unknown::Known(Ability::Analytic);
    mon_p2.min_pre_nature_stat[1] = true_atk_bsv;
    mon_p2.max_pre_nature_stat[1] = true_atk_bsv;
    mon_p2.min_stats[1] = true_atk_bsv;
    mon_p2.max_stats[1] = true_atk_bsv;

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
            vec![event(EventKind::DamageDealt { max_hp: 0, target: p1(0), new_hp: new_hp_val })],
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
            EventKind::Switch(SwitchState { disguise_species: None, max_hp: 0,
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
        event(EventKind::Switch(SwitchState { disguise_species: None, max_hp: 0,
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
            EventKind::Switch(SwitchState { disguise_species: None, max_hp: 0,
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

/// C5: a Sand Stream mon switching in under an already-active NORMAL Sandstorm (not just
/// strong/primordial weather, per C4) must neither be excluded nor trigger a contradiction
/// panic. Since the simulator fix, `apply_entry_ability_field_effects` skips `set_weather`
/// entirely when the ability's target weather already matches (so the turn counter isn't
/// reset) — meaning no `WeatherChanged` fires even though the weather is perfectly ordinary,
/// not strong/primordial. The absence-inference guard must therefore be per-ability (does
/// THIS ability's target weather match current weather), not just a blanket
/// strong-weather check — otherwise it would wrongly exclude SandStream here, moments before
/// the unconditional `AbilityRevealed{SandStream}` reaction tries to set it Known.
#[test]
fn test_c5_weather_setter_not_excluded_under_same_normal_weather() {
    // State: ordinary Sandstorm already active (NOT strong/primordial weather).
    let mut state = battle_with_p2(vec![unknown_mon()]);
    state.weather = Some(Weather::Sandstorm);

    // P2 switches in with SandStream revealed (AbilityRevealed reaction) but NO WeatherChanged
    // — set_weather no-ops because Sandstorm is already active.
    let events = vec![
        event_with(
            EventKind::Switch(SwitchState { disguise_species: None, max_hp: 0,
                slot: p2(0),
                species: Species::Tyranitar,
                level: 50,
                hp: PokemonHP::Percent(100),
                status: None,
                tera_type: None,
            }),
            vec![
                event_with(
                    EventKind::AbilityRevealed { slot: p2(0), ability: Ability::SandStream },
                    vec![], // No WeatherChanged reaction (already-active weather suppressed it).
                ),
            ],
        ),
    ];

    // Must not panic (a blanket strong-weather-only guard would exclude SandStream here, then
    // AbilityRevealed would try to set Known(SandStream) → contradiction panic).
    let result = apply(state, events);

    let p2_mon = &result.p2_active_mons[0];
    assert!(
        !unknown_is_excluded(&p2_mon.possible_abilities, &Ability::SandStream),
        "SandStream must NOT be excluded when switching in under an already-active Sandstorm; \
         possible_abilities = {:?}", p2_mon.possible_abilities
    );

    // Contrast: a DIFFERENT weather-setting ability (Drizzle, sets Rain, not Sandstorm) is
    // still soundly excludable here — its absence of WeatherChanged remains real evidence,
    // since Drizzle WOULD have changed Sandstorm to Rain and didn't.
    assert!(
        unknown_is_excluded(&p2_mon.possible_abilities, &Ability::Drizzle),
        "Drizzle should still be excluded — it would have changed the weather and didn't; \
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
    use crate::information::unknowns::{Statement, Unknown, UnknownBattleState, UnknownMatchState, UnknownPokemonState};
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

    /// Like [`fog_1v1`] but seeds P2's belief via [`UnknownPokemonState::from_opponent_open_sheet`]
    /// with `force_max_ivs=true` — the exact server code path (`create_battle`'s
    /// `informationMode="openSheet"` branch, via `team_preview_open_sheet_from_perspective`)
    /// that produced the S34/S35 contradictions. `reveal_nature=false` matches plain
    /// "Open Team Sheet" (not "+ Natures").
    fn fog_1v1_open_sheet_force_max_ivs(p1: &PokemonState, p2: &PokemonState) -> UnknownBattleState {
        let dex = pokemon_dex();
        super::battle_nvn(
            vec![UnknownPokemonState::from_known_pokemon(p1)],
            vec![UnknownPokemonState::from_opponent_open_sheet(p2, dex, 50, false, true)],
        )
    }

    /// S34/S35 regression: a real, high-frequency (~1/3 to 1/2 of turns, depending on
    /// the damage roll) crash found in production-realistic "Open Team Sheet" gameplay.
    ///
    /// S34: `from_opponent_open_sheet`/`from_opponent_species` always seeded the min
    /// side of `min_pre_nature_stat`/`min_stats` at IV 0, with no way to know the format
    /// pins opponent IVs to 31 (`force_max_ivs`) — leaving a "phantom" IV-0-consistent
    /// region that Direction B's damage back-solve could narrow into, but `pass5_back_solve`
    /// (which correctly restricts its own search to IV 31 per `config.force_max_ivs`)
    /// could never satisfy.
    ///
    /// S35: independently, `emit_nature_conditional_bounds` force-applied a per-nature-class
    /// conditional bound directly onto `min_pre_nature_stat`/`max_pre_nature_stat` whenever
    /// no gear escape existed — completely dropping the `not_kappa_guards` condition (i.e.
    /// "this nature class is actually confirmed"). Since every nature class's own clause hit
    /// this shortcut independently, a nerf class's high lower-bound and a boost class's low
    /// upper-bound could BOTH get force-applied for the same mon/stat, inverting
    /// `min_pre_nature_stat` above `max_pre_nature_stat`.
    ///
    /// Either bug alone crashes `pass5_back_solve`'s "every candidate nature is infeasible"
    /// soundness assertion. Exhaustively checks all 16 damage rolls (deterministic — the
    /// original bug was only stochastically hit depending on which roll landed, so a single
    /// fixed roll would not reliably catch a regression here).
    #[test]
    fn test_s34_s35_open_sheet_force_max_ivs_no_pass5_contradiction() {
        let pd = pokemon_dex();
        let md = move_dex();

        let p1 = build_pokemon_state(
            Species::Garchomp, pd, md, Some(50),
            Some([Some(PokemonMove::Earthquake), None, None, None]),
            None, Some(Ability::RoughSkin), Some(Nature::Adamant), Some(Item::ChoiceBand),
            None, Some([4, 252, 0, 0, 0, 252]), None, true,
        );
        let p2 = build_pokemon_state(
            Species::Snorlax, pd, md, Some(50),
            Some([Some(PokemonMove::BodySlam), None, None, None]),
            None, Some(Ability::ThickFat), Some(Nature::Impish), Some(Item::None),
            None, Some([252, 0, 252, 0, 4, 0]), None, true,
        );

        let battle = battle_state_from_lists(vec![p1.clone()], vec![], vec![p2.clone()], vec![]);
        let state = MatchState::BattleState(battle);
        let p1_cmd = PlayerCommand::Battle(simple_attack(Player::P1, vec![0]));
        let p2_cmd = PlayerCommand::Battle(simple_attack(Player::P2, vec![0]));

        let fog = fog_1v1_open_sheet_force_max_ivs(&p1, &p2);

        let branches = crate::tests::simuilator_test_helpers::run_single_turn_with_events_opts(
            &state, &p1_cmd, &p2_cmd, md, pd, Player::P1, true, 16,
        );
        assert!(!branches.is_empty());

        let mut checked = 0;
        for (st, events_opt, _prob) in branches {
            if !matches!(st, MatchState::BattleState(_)) {
                continue; // A one-shot faint on this roll — nothing further to check.
            }
            let Some(events) = events_opt else { continue };
            checked += 1;
            // Must not panic — this is the actual regression under test.
            apply_information(
                UnknownMatchState::Battle(fog.clone()),
                &events,
                false,
                pd,
                md,
                &HashMap::new(),
                &InferenceConfig::default(),
            );
        }
        assert!(
            checked > 1,
            "expected multiple distinct damage-roll branches to be exercised, got {checked}"
        );
    }

    /// S37 regression: the team-preview→battle transition (`into_battle_state`) places
    /// P1's leads directly into `p1_active_mons` with their full `Known` nature/EV/IV
    /// data from `from_known_pokemon` — but that same transition's own event log
    /// (`session.rs::resolve_turn`'s documented two-step flow) includes a
    /// `SimultaneousSwitch` for BOTH sides' leads, P1's own sent-out reveal alongside
    /// P2's. Before this fix, `pass1_switch` processed P1's own `SimultaneousSwitch`
    /// entry exactly like a genuine mid-battle switch: look up the incoming species in
    /// `p1_known_back_mons`/`p1_possible_back_mons` (which only ever hold P1's BENCHED
    /// mons — a lead is already active, never in either list), fail to find it, and
    /// fall through to building a brand-new `from_opponent_species`-style mon —
    /// DISCARDING the correct, fully-known entry `into_battle_state` had already placed
    /// and replacing it with a wide-uncertain one (`possible_natures = Not([])`,
    /// `min_evs`/`max_evs` widened to `[0, 252]`). This corrupted P1's OWN belief on
    /// every real battle that goes through the actual server flow — the earlier
    /// `test_s34_s35_*` test above didn't catch it because it built the fog state
    /// directly, bypassing `into_battle_state` entirely. Once a mon later Mega Evolves
    /// (`recompute_stat_bounds_for_species_change` re-derives `min_stats`/`max_stats`
    /// from the now-wrongly-wide nature/EV window), this produced a visibly WIDE stat
    /// range for a mon that should be a single exact value, feeding wrong data into
    /// Direction A/B and eventually an "every candidate nature is infeasible" pass5
    /// contradiction or a `SpeedComparison` inversion.
    #[test]
    fn test_s37_p1_own_lead_stays_known_through_team_preview_transition() {
        let pd = pokemon_dex();
        let md = move_dex();

        let p1a = build_pokemon_state(
            Species::Tyranitar, pd, md, Some(50),
            Some([Some(PokemonMove::RockSlide), None, None, None]),
            None, Some(Ability::SandStream), Some(Nature::Adamant), Some(Item::Tyranitarite),
            None, Some([26, 252, 0, 0, 0, 0]), None, true,
        );
        let p2a = build_pokemon_state(
            Species::Snorlax, pd, md, Some(50),
            Some([Some(PokemonMove::BodySlam), None, None, None]),
            None, Some(Ability::ThickFat), Some(Nature::Impish), Some(Item::None),
            None, Some([252, 0, 252, 0, 4, 0]), None, true,
        );

        let preview = UnknownMatchState::team_preview_open_sheet_from_perspective(
            Player::P1, std::slice::from_ref(&p1a), std::slice::from_ref(&p2a), pd, 1, 1, 50,
            crate::information::unknowns::InformationMode::OpenTeamSheet, true,
        );
        let UnknownMatchState::TeamPreview(preview) = preview else {
            panic!("expected TeamPreview")
        };
        let fog = preview.into_battle_state(Player::P1, &[0], &[], &[0], &[]);

        // The exact event shape the real server emits for this transition: both
        // leads sent out simultaneously.
        let events = vec![InformationEvent {
            kind: crate::information::information::EventKind::SimultaneousSwitch {
                switches: vec![
                    crate::information::information::SwitchState { disguise_species: None, max_hp: 0,
                        slot: super::p1(0),
                        species: Species::Tyranitar,
                        level: 50,
                        hp: crate::information::unknowns::PokemonHP::Number(p1a.hp),
                        status: None,
                        tera_type: None,
                    },
                    crate::information::information::SwitchState { disguise_species: None, max_hp: 0,
                        slot: super::p2(0),
                        species: Species::Snorlax,
                        level: 50,
                        hp: crate::information::unknowns::PokemonHP::Percent(100),
                        status: None,
                        tera_type: None,
                    },
                ],
            },
            reactions: vec![],
        }];

        let result = apply_information(
            UnknownMatchState::Battle(fog), &events, true, pd, md, &HashMap::new(),
            &InferenceConfig::default(),
        );
        let UnknownMatchState::Battle(b) = result else {
            panic!("expected Battle state")
        };
        let p1_mon = &b.p1_active_mons[0];

        assert_eq!(
            p1_mon.possible_natures, Unknown::Known(Nature::Adamant),
            "P1's own lead must keep its Known nature through the team-preview \
             transition; got {:?}", p1_mon.possible_natures
        );
        assert_eq!(
            p1_mon.min_evs, p1a.evs,
            "P1's own lead must keep its exact EVs, not widen to [0,252]; got {:?}",
            p1_mon.min_evs
        );
        assert_eq!(p1_mon.max_evs, p1a.evs);
        assert_eq!(
            p1_mon.min_stats[1], p1_mon.max_stats[1],
            "P1's own lead's Atk must stay collapsed to a single exact value \
             ([{}, {}]), not a range",
            p1_mon.min_stats[1], p1_mon.max_stats[1]
        );
    }

    /// S37 (generalized) regression: the guard in `pass1_switch` that detects "this
    /// lead was already pre-placed by `into_battle_state`, don't rebuild it" used to
    /// be hardcoded to `Player::P1` — correct for a P1 belief (where P1 is the
    /// viewer, pre-placed fully known), but silently wrong for a P2 belief, where
    /// it's P2's OWN leads that are pre-placed. Before the fix, this exact test
    /// (mirroring `test_s37_p1_own_lead_stays_known_through_team_preview_transition`
    /// but for `viewer = Player::P2`) would fail: P2's own Tyranitar would fall
    /// through to the "completely new mon" branch and get rebuilt as a
    /// wide-uncertain `from_opponent_species` mon, discarding the real nature/EV
    /// knowledge the engine already had about the player's own team.
    #[test]
    fn test_s37_p2_own_lead_stays_known_through_team_preview_transition() {
        let pd = pokemon_dex();
        let md = move_dex();

        let p2a = build_pokemon_state(
            Species::Tyranitar, pd, md, Some(50),
            Some([Some(PokemonMove::RockSlide), None, None, None]),
            None, Some(Ability::SandStream), Some(Nature::Adamant), Some(Item::Tyranitarite),
            None, Some([26, 252, 0, 0, 0, 0]), None, true,
        );
        let p1a = build_pokemon_state(
            Species::Snorlax, pd, md, Some(50),
            Some([Some(PokemonMove::BodySlam), None, None, None]),
            None, Some(Ability::ThickFat), Some(Nature::Impish), Some(Item::None),
            None, Some([252, 0, 252, 0, 4, 0]), None, true,
        );

        let preview = UnknownMatchState::team_preview_open_sheet_from_perspective(
            Player::P2, std::slice::from_ref(&p2a), std::slice::from_ref(&p1a), pd, 1, 1, 50,
            crate::information::unknowns::InformationMode::OpenTeamSheet, true,
        );
        let UnknownMatchState::TeamPreview(preview) = preview else {
            panic!("expected TeamPreview")
        };
        let fog = preview.into_battle_state(Player::P2, &[], &[], &[0], &[]);

        // The exact event shape the real server emits for this transition: both
        // leads sent out simultaneously.
        let events = vec![InformationEvent {
            kind: crate::information::information::EventKind::SimultaneousSwitch {
                switches: vec![
                    crate::information::information::SwitchState { disguise_species: None, max_hp: 0,
                        slot: super::p1(0),
                        species: Species::Snorlax,
                        level: 50,
                        hp: crate::information::unknowns::PokemonHP::Percent(100),
                        status: None,
                        tera_type: None,
                    },
                    crate::information::information::SwitchState { disguise_species: None, max_hp: 0,
                        slot: super::p2(0),
                        species: Species::Tyranitar,
                        level: 50,
                        hp: crate::information::unknowns::PokemonHP::Number(p2a.hp),
                        status: None,
                        tera_type: None,
                    },
                ],
            },
            reactions: vec![],
        }];

        let result = apply_information(
            UnknownMatchState::Battle(fog), &events, true, pd, md, &HashMap::new(),
            &InferenceConfig::default(),
        );
        let UnknownMatchState::Battle(b) = result else {
            panic!("expected Battle state")
        };
        let p2_mon = &b.p2_active_mons[0];

        assert_eq!(
            p2_mon.possible_natures, Unknown::Known(Nature::Adamant),
            "P2's own lead must keep its Known nature through the team-preview \
             transition; got {:?}", p2_mon.possible_natures
        );
        assert_eq!(
            p2_mon.min_evs, p2a.evs,
            "P2's own lead must keep its exact EVs, not widen to [0,252]; got {:?}",
            p2_mon.min_evs
        );
        assert_eq!(p2_mon.max_evs, p2a.evs);
        assert_eq!(
            p2_mon.min_stats[1], p2_mon.max_stats[1],
            "P2's own lead's Atk must stay collapsed to a single exact value \
             ([{}, {}]), not a range",
            p2_mon.min_stats[1], p2_mon.max_stats[1]
        );
        // `bench_outgoing_mon`'s companion guard must also be generalized to P2 —
        // otherwise it would still clone P2's own lead onto the bench (since ITS
        // guard wouldn't fire), and `pass1_switch`'s S37 shortcut returning early
        // (without consuming a bench match) would leave that clone orphaned in
        // `p2_known_back_mons`, duplicating the same physical Pokémon across both
        // lists (mirrors `test_s37_own_lead_not_duplicated_onto_bench` for P1).
        assert!(
            b.p2_known_back_mons.is_empty(),
            "P2's own lead must not be duplicated onto the bench during the \
             team-preview transition; got {:?}", b.p2_known_back_mons
        );
    }

    /// S37 companion regression: `bench_outgoing_mon` used to clone the slot's current
    /// occupant onto the bench UNCONDITIONALLY, before `pass1_switch` ran — relying on
    /// `pass1_switch` to consume the matching bench entry once it placed the incoming
    /// mon into the active slot. But at the team-preview→battle transition, P1's own
    /// lead-reveal `SimultaneousSwitch` hits the S37 guard in `pass1_switch` (the
    /// incoming species already matches the slot's occupant, since `into_battle_state`
    /// pre-placed the lead there) and returns EARLY, never reaching the bench-removal.
    /// The clone `bench_outgoing_mon` had already pushed was left orphaned in
    /// `p1_known_back_mons` — duplicating one physical Pokémon across both the active
    /// slot and the bench. `teammate_indices`/`enforce_unique_item` then see two
    /// distinct "teammates" both holding the lead's item, and a later `ItemRevealed`
    /// re-confirming that item on the (real) active mon panics excluding it from the
    /// duplicate, which is already `Known` to the same item (a false item-clause
    /// contradiction — this exact shape was reported with a Leftovers Corviknight).
    #[test]
    fn test_s37_own_lead_not_duplicated_onto_bench() {
        let pd = pokemon_dex();
        let md = move_dex();

        let p1a = build_pokemon_state(
            Species::Corviknight, pd, md, Some(50),
            Some([Some(PokemonMove::BraveBird), None, None, None]),
            None, Some(Ability::Pressure), Some(Nature::Impish), Some(Item::Leftovers),
            None, Some([252, 0, 252, 0, 4, 0]), None, true,
        );
        let p2a = build_pokemon_state(
            Species::Snorlax, pd, md, Some(50),
            Some([Some(PokemonMove::BodySlam), None, None, None]),
            None, Some(Ability::ThickFat), Some(Nature::Impish), Some(Item::None),
            None, Some([252, 0, 252, 0, 4, 0]), None, true,
        );

        let preview = UnknownMatchState::team_preview_open_sheet_from_perspective(
            Player::P1, std::slice::from_ref(&p1a), std::slice::from_ref(&p2a), pd, 1, 1, 50,
            crate::information::unknowns::InformationMode::OpenTeamSheet, true,
        );
        let UnknownMatchState::TeamPreview(preview) = preview else {
            panic!("expected TeamPreview")
        };
        // 1 active, 0 back — the whole roster for this test is the single lead.
        let fog = preview.into_battle_state(Player::P1, &[0], &[], &[0], &[]);

        let events = vec![InformationEvent {
            kind: crate::information::information::EventKind::SimultaneousSwitch {
                switches: vec![
                    crate::information::information::SwitchState { disguise_species: None, max_hp: 0,
                        slot: super::p1(0),
                        species: Species::Corviknight,
                        level: 50,
                        hp: crate::information::unknowns::PokemonHP::Number(p1a.hp),
                        status: None,
                        tera_type: None,
                    },
                    crate::information::information::SwitchState { disguise_species: None, max_hp: 0,
                        slot: super::p2(0),
                        species: Species::Snorlax,
                        level: 50,
                        hp: crate::information::unknowns::PokemonHP::Percent(100),
                        status: None,
                        tera_type: None,
                    },
                ],
            },
            reactions: vec![],
        }];

        let result = apply_information(
            UnknownMatchState::Battle(fog), &events, true, pd, md, &HashMap::new(),
            &InferenceConfig::default(),
        );
        let UnknownMatchState::Battle(b) = result else {
            panic!("expected Battle state")
        };

        assert!(
            b.p1_known_back_mons.is_empty(),
            "P1's lead must not be duplicated onto the bench during the team-preview \
             transition; got {:?}", b.p1_known_back_mons
        );

        // Regression proper: a later ItemRevealed re-confirming the lead's own item
        // must not panic. Before the fix, the orphaned bench duplicate (also
        // Known(Leftovers)) collided with this exclusion under item clause.
        let item_reveal = vec![InformationEvent {
            kind: crate::information::information::EventKind::ItemRevealed {
                slot: super::p1(0),
                item: Item::Leftovers,
            },
            reactions: vec![],
        }];
        let result2 = apply_information(
            UnknownMatchState::Battle(b), &item_reveal, true, pd, md, &HashMap::new(),
            &InferenceConfig::default(),
        );
        let UnknownMatchState::Battle(b2) = result2 else {
            panic!("expected Battle state")
        };
        assert_eq!(b2.p1_active_mons[0].item, Unknown::Known(Item::Leftovers));
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

    /// TODO.md soundness regression: same shape as
    /// `roundtrip_b_p1_attacks_p2_def_stat_soundness`, but P2's true EVs are maxed on
    /// HP (252) and zero on Def — the exact shape of the reported "assumes Def EVs
    /// when the truth is HP EVs" concern. Pass 3 Direction A back-solves the
    /// defender's Def BSV by unioning the feasible-BSV interval over every achievable
    /// (HP, Def) pair (`achievable_defender_hp_values` × `find_feasible_bsv_range_a`);
    /// since the true HP is always among the enumerated candidates, the pair
    /// containing the true (HP, Def) always contributes its own interval to that
    /// union, so the resulting marginal Def bound provably still contains the truth
    /// regardless of which HP hypothesis is real — investigated and confirmed sound
    /// by inspection AND empirically here. If a future change to Pass 3/Pass 5/
    /// `ev_total_cap` breaks that coupling, this fails with either `min_evs[2] > 0`
    /// (wrongly excluding the true Def EV) or `max_evs[0] < 252` (wrongly excluding
    /// the true HP EV).
    #[test]
    fn roundtrip_b1_p1_attacks_p2_hp_ev_vs_def_ev_soundness() {
        let pd = pokemon_dex();
        let md = move_dex();

        let p1 = build_pokemon_state(
            Species::Garchomp, pd, md, Some(50),
            Some([Some(PokemonMove::Earthquake), Some(PokemonMove::Splash), None, None]),
            None, Some(Ability::None), Some(Nature::Hardy), None, None,
            Some([0u8; 6]), Some([31u8; 6]), false,
        );

        // True build: 252 HP EVs, 0 Def EVs (all other EVs 0 too) — maximally
        // HP-heavy, the exact shape the TODO complaint describes.
        let p2 = build_pokemon_state(
            Species::Snorlax, pd, md, Some(50),
            Some([Some(PokemonMove::Splash), None, None, None]),
            None, Some(Ability::Immunity), Some(Nature::Hardy), None, None,
            Some([252u8, 0, 0, 0, 0, 0]), Some([31u8; 6]), false,
        );
        let true_hp_ev: u8 = p2.evs[0];
        let true_def_ev: u8 = p2.evs[2];
        assert_eq!(true_hp_ev, 252);
        assert_eq!(true_def_ev, 0);

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
            p2_fog.min_evs[0] <= true_hp_ev && true_hp_ev <= p2_fog.max_evs[0],
            "soundness: true HP EV ({true_hp_ev}) must lie within inferred range \
             [{}, {}]", p2_fog.min_evs[0], p2_fog.max_evs[0]
        );
        assert!(
            p2_fog.min_evs[2] <= true_def_ev && true_def_ev <= p2_fog.max_evs[2],
            "soundness: true Def EV ({true_def_ev}) must lie within inferred range \
             [{}, {}] — HP EV bound was [{}, {}]",
            p2_fog.min_evs[2], p2_fog.max_evs[2], p2_fog.min_evs[0], p2_fog.max_evs[0]
        );
    }

    /// TODO.md soundness regression, two-hit variant: two consecutive Direction-A
    /// observations against the same HP-heavy defender (252 HP EV / 0 Def EV), belief
    /// threaded turn-to-turn exactly as `session::resolve_turn` does it — checks that
    /// accumulating a second percent-HP delta on top of the first never tightens past
    /// the true (HP, Def) pair once both observations are intersected. See
    /// `roundtrip_b1_p1_attacks_p2_hp_ev_vs_def_ev_soundness` for why each individual
    /// observation's bound is sound; intersecting two sound bounds (each already
    /// containing the truth) can only ever stay sound.
    #[test]
    fn roundtrip_b2_two_hits_hp_ev_vs_def_ev_soundness() {
        let pd = pokemon_dex();
        let md = move_dex();

        let p1 = build_pokemon_state(
            Species::Garchomp, pd, md, Some(50),
            Some([Some(PokemonMove::Earthquake), Some(PokemonMove::Splash), None, None]),
            None, Some(Ability::None), Some(Nature::Hardy), None, None,
            Some([0u8; 6]), Some([31u8; 6]), false,
        );
        let p2 = build_pokemon_state(
            Species::Snorlax, pd, md, Some(50),
            Some([Some(PokemonMove::Splash), None, None, None]),
            None, Some(Ability::Immunity), Some(Nature::Hardy), None, None,
            Some([252u8, 0, 0, 0, 0, 0]), Some([31u8; 6]), false,
        );
        let true_hp_ev: u8 = p2.evs[0];
        let true_def_ev: u8 = p2.evs[2];

        let battle = battle_state_from_lists(vec![p1.clone()], vec![], vec![p2.clone()], vec![]);
        let state = MatchState::BattleState(battle);
        let p1_cmd = PlayerCommand::Battle(simple_attack(Player::P1, vec![0]));
        let p2_cmd = PlayerCommand::Battle(simple_attack(Player::P2, vec![0]));

        // Turn 1.
        let mut branches =
            run_single_turn_with_events(&state, &p1_cmd, &p2_cmd, md, pd, Player::P1);
        branches.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
        let (turn1_state, turn1_events, _) = branches
            .into_iter()
            .find(|(st, _, _)| matches!(st, MatchState::BattleState(_)))
            .expect("no BattleState branch found after turn 1 — P2 may have fainted");
        let turn1_events = turn1_events.expect("observer set → events must be Some");

        let fog = fog_1v1(&p1, Species::Snorlax);
        let dex = pokemon_dex();
        let belief_after_1 = apply_information(
            UnknownMatchState::Battle(fog),
            &turn1_events,
            false,
            dex,
            md,
            &HashMap::new(),
            &InferenceConfig::default(),
        );

        // Turn 2: same attack again, from the post-turn-1 ground truth.
        let mut branches2 =
            run_single_turn_with_events(&turn1_state, &p1_cmd, &p2_cmd, md, pd, Player::P1);
        branches2.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
        let Some((_turn2_state, turn2_events, _)) = branches2
            .into_iter()
            .find(|(st, _, _)| matches!(st, MatchState::BattleState(_)))
        else {
            // P2 fainted on turn 2 — nothing further to check; turn 1's bound alone
            // was already verified sound by the single-hit probe.
            return;
        };
        let turn2_events = turn2_events.expect("observer set → events must be Some");

        let belief_after_2 = apply_information(
            belief_after_1, &turn2_events, false, dex, md, &HashMap::new(),
            &InferenceConfig::default(),
        );
        let UnknownMatchState::Battle(result) = belief_after_2 else {
            panic!("expected Battle state after second apply_information");
        };

        let p2_fog = &result.p2_active_mons[0];
        assert!(
            p2_fog.min_evs[0] <= true_hp_ev && true_hp_ev <= p2_fog.max_evs[0],
            "soundness (2 hits): true HP EV ({true_hp_ev}) must lie within inferred \
             range [{}, {}]", p2_fog.min_evs[0], p2_fog.max_evs[0]
        );
        assert!(
            p2_fog.min_evs[2] <= true_def_ev && true_def_ev <= p2_fog.max_evs[2],
            "soundness (2 hits): true Def EV ({true_def_ev}) must lie within inferred \
             range [{}, {}] — HP EV bound was [{}, {}]",
            p2_fog.min_evs[2], p2_fog.max_evs[2], p2_fog.min_evs[0], p2_fog.max_evs[0]
        );
    }

    // ── Regression: promotion must never leave a stale disguise-derived HP window ──
    //
    // Under the parallel-hypothesis model, a hypothesis is tracked against its OWN
    // species' base stats from the moment it's seeded (never the disguise's) — so
    // promoting it (via `IllusionEnded`) can never inherit a stale, disguise-derived
    // stat window the way the old `HasSpecies`-forced-mid-BCP path could (S30). This
    // drives that scenario with the REAL dex (Snorlax base HP 160 vs. Zoroark base HP
    // 60) end-to-end and asserts the resulting window is Zoroark-consistent.
    #[test]
    fn test_zoroark_promotion_never_leaves_stale_disguise_hp_window() {
        use crate::state::pokemon::calc_hp;
        let pd = pokemon_dex();

        let zoroark_back = UnknownPokemonState::from_opponent_species(Species::Zoroark, pd, 50);
        let mut snorlax_back = UnknownPokemonState::from_opponent_species(Species::Snorlax, pd, 50);
        snorlax_back.possible_illusion_state = Some(Box::new(
            crate::information::unknowns::seed_illusion_hypothesis_for(&snorlax_back, &zoroark_back),
        ));
        let mut state = super::battle_with_p2(vec![]);
        state.p2_known_back_mons = vec![snorlax_back];
        state.p2_possible_back_mons = vec![zoroark_back];
        state.p2_slot_conditions = vec![vec![]];
        state.p2_unresolved_zoroark_count = 1;

        let after_switch = apply_information(
            UnknownMatchState::Battle(state),
            &[super::switch_in(Species::Snorlax, super::p2(0))],
            false,
            pd,
            &HashMap::new(),
            &HashMap::new(),
            &InferenceConfig::default(),
        );
        let UnknownMatchState::Battle(after_switch) = after_switch else {
            panic!("expected battle state")
        };

        // The disguise breaks (direct-damage reveal): promote to the true identity.
        let result = apply_information(
            UnknownMatchState::Battle(after_switch),
            &[super::event(super::EventKind::IllusionEnded {
                slot: super::p2(0),
                actual_species: Species::Zoroark,
            })],
            false,
            pd,
            &HashMap::new(),
            &HashMap::new(),
            &InferenceConfig::default(),
        );
        let UnknownMatchState::Battle(b) = result else { panic!("expected battle state") };
        let m = &b.p2_active_mons[0];

        assert_eq!(m.possible_species, Unknown::Known(Species::Zoroark));
        assert!(
            m.min_stats[0] <= m.max_stats[0],
            "HP window must be non-empty after promotion: [{}, {}]",
            m.min_stats[0], m.max_stats[0]
        );
        // The window must be Zoroark-consistent: a real Zoroark (base HP 60, 31 IV,
        // any EV) must lie within it, NOT Snorlax's (base HP 160) window.
        let real_zoroark_hp = calc_hp(60, 31, 0, 50);
        assert!(
            m.min_stats[0] <= real_zoroark_hp && real_zoroark_hp <= m.max_stats[0],
            "promoted HP window [{}, {}] must contain a real Zoroark HP ({}) — a stale \
             Snorlax-derived window would exclude it",
            m.min_stats[0], m.max_stats[0], real_zoroark_hp
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
    /// `[min_stats[5], max_stats[5]]` after Pass 4's bound propagation.
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
            p2_fog.min_stats[5] <= true_spe_bsv && true_spe_bsv <= p2_fog.max_stats[5],
            "soundness (Pass 4): true Spe BSV ({true_spe_bsv}) must lie within \
             inferred Spe range [{}, {}]",
            p2_fog.min_stats[5], p2_fog.max_stats[5]
        );
        assert!(
            !unknown_is_excluded(&p2_fog.possible_abilities, &Ability::Immunity),
            "soundness: true ability Immunity must not be excluded"
        );
    }

    // ── Scenario S28: Analytic fires when the attacker moves last after a switch ──

    /// P1 switches (a switch resolves before any move), then P2's Analytic Garchomp
    /// attacks the replacement as the turn's only mover — so Analytic's ×1.3 applied
    /// in the real sim. Because P1 used no move, the old target-centric heuristic
    /// ("did the target already move this turn?") concluded Analytic did NOT fire and
    /// dropped it from the Direction-B booster union, forcing the ×1.3-inflated
    /// observed damage to be explained by a higher attacker Atk BSV — excluding the
    /// true value (Pass 5 then panics: "every candidate nature infeasible").
    ///
    /// Roundtrip so the observed damage is guaranteed consistent with the true world
    /// (Atk BSV of a real 252-Atk Adamant Garchomp under Analytic).
    #[test]
    fn roundtrip_s28_analytic_last_mover_after_switch() {
        let pd = pokemon_dex();
        let md = move_dex();

        // P1 lead: bulky Snorlax that will switch out. Back: a second wall to receive
        // the hit (Def known to us; its species base Def anchors the inversion).
        let p1_lead = build_pokemon_state(
            Species::Snorlax, pd, md, Some(50),
            Some([Some(PokemonMove::Splash), None, None, None]),
            None, Some(Ability::Immunity), Some(Nature::Hardy), None, None,
            Some([0u8; 6]), Some([31u8; 6]), false,
        );
        let p1_back = build_pokemon_state(
            Species::Regice, pd, md, Some(50),
            Some([Some(PokemonMove::Splash), None, None, None]),
            None, Some(Ability::ClearBody), Some(Nature::Hardy), None, None,
            Some([0u8; 6]), Some([31u8; 6]), false,
        );

        // P2: Garchomp, Analytic, NEUTRAL 0-EV Atk (true BSV near the species floor).
        // A max-Atk attacker would not expose the bug: without Analytic the observed
        // ×1.3 damage would demand a BSV above Garchomp's cap, giving an empty feasible
        // set (no narrowing) instead of an above-truth min. A low true BSV keeps the
        // wrong (no-Analytic) requirement inside the range so it raises the floor.
        // A weak, non-STAB move (Tackle, 40 BP) so the exact-damage inversion is
        // discriminating; a strong STAB move saturates (every BSV reproduces it).
        let p2 = build_pokemon_state(
            Species::Garchomp, pd, md, Some(50),
            Some([Some(PokemonMove::Tackle), None, None, None]),
            None, Some(Ability::Analytic), Some(Nature::Hardy), None, None,
            Some([0u8; 6]), Some([31u8; 6]), false,
        );
        // Hardy is neutral → pre-nature BSV == the final Atk stat.
        let true_atk_bsv = p2.stats[1];

        let battle = battle_state_from_lists(
            vec![p1_lead.clone()], vec![p1_back.clone()],
            vec![p2.clone()], vec![],
        );
        let state = MatchState::BattleState(battle);

        // P1 switches to Regice (back-index 0); P2 attacks — P2 moves last.
        let p1_cmd = PlayerCommand::Battle(vec![
            BattleCommand::Switch(SwitchCommand { party_index: 0 }),
        ]);
        let p2_cmd = PlayerCommand::Battle(simple_attack(Player::P2, vec![0]));

        let events = simulate_and_get_events(state, p1_cmd, p2_cmd);

        // Fog: P1 fully known (lead + back); P2 Garchomp with its ability narrowed so
        // Analytic is the ONLY offensive booster candidate (SandVeil is defensive).
        // Without this the union's stronger boosters (Huge Power, Adaptability, …)
        // would dominate the min-BSV and mask whether Analytic is (in)correctly
        // included — the S28 distinction only bites when Analytic is the binding
        // booster.
        let mut p2_fog_mon =
            UnknownPokemonState::from_opponent_species(Species::Garchomp, pd, 50);
        p2_fog_mon.possible_abilities =
            Unknown::Possibly(vec![Ability::Analytic, Ability::SandVeil]);
        p2_fog_mon.possible_original_abilities =
            Unknown::Possibly(vec![Ability::Analytic, Ability::SandVeil]);
        // Pin the item to None too, else the item union (Choice Band ×1.5, Life Orb
        // ×1.3, …) would cover for Analytic and mask the distinction.
        p2_fog_mon.item = Unknown::Known(Item::None);
        let mut initial_fog = super::battle_nvn(
            vec![UnknownPokemonState::from_known_pokemon(&p1_lead)],
            vec![p2_fog_mon],
        );
        initial_fog.p1_known_back_mons =
            vec![UnknownPokemonState::from_known_pokemon(&p1_back)];

        let result = apply_information(
            UnknownMatchState::Battle(initial_fog),
            &events, false, pd, md, &HashMap::new(), &InferenceConfig::default(),
        );
        let result = match result {
            UnknownMatchState::Battle(b) => b,
            _ => panic!("expected Battle state"),
        };

        let p2_slot = super::FieldSlot { player: Player::P2, slot_index: 0 };
        let p2_idx = super::mon_idx_for_active_slot(&result, &p2_slot).unwrap();
        let p2_fog = super::get_mon_by_idx(&result, p2_idx).unwrap();
        assert!(
            p2_fog.min_pre_nature_stat[1] <= true_atk_bsv
                && true_atk_bsv <= p2_fog.max_pre_nature_stat[1],
            "soundness (S28): true Atk BSV {true_atk_bsv} must lie within inferred \
             pre-nature range [{}, {}] — Analytic must be in the union because P2 \
             moved last (after P1's switch)",
            p2_fog.min_pre_nature_stat[1], p2_fog.max_pre_nature_stat[1]
        );
    }

    // ── Scenario S26: Transform (Imposter) copies the source's fog identity ──────

    /// P2's Ditto (Imposter) switches in opposite P1's fully-known Garchomp and
    /// transforms. The real sim emits a `Transformed` event; inference must overlay
    /// Garchomp's identity onto the Ditto fog entry — and because the copy source is
    /// the observer's OWN Known mon, the copied non-HP stats become exact.
    #[test]
    fn roundtrip_s26_imposter_copies_known_source_exactly() {
        let pd = pokemon_dex();
        let md = move_dex();

        // P1: known Garchomp (max Atk, Adamant) with Splash.
        let p1 = build_pokemon_state(
            Species::Garchomp, pd, md, Some(50),
            Some([Some(PokemonMove::Splash), None, None, None]),
            None, Some(Ability::SandVeil), Some(Nature::Adamant), None, None,
            Some([0, 252, 0, 0, 0, 4]), Some([31u8; 6]), false,
        );
        let true_atk = p1.stats[1];

        // P2 lead: Snorlax; back: Ditto with Imposter.
        let p2_lead = build_pokemon_state(
            Species::Snorlax, pd, md, Some(50),
            Some([Some(PokemonMove::Splash), None, None, None]),
            None, Some(Ability::Immunity), Some(Nature::Hardy), None, None,
            Some([0u8; 6]), Some([31u8; 6]), false,
        );
        let p2_ditto = build_pokemon_state(
            Species::Ditto, pd, md, Some(50),
            Some([Some(PokemonMove::Transform), None, None, None]),
            None, Some(Ability::Imposter), Some(Nature::Hardy), None, None,
            Some([0u8; 6]), Some([31u8; 6]), false,
        );

        let battle = battle_state_from_lists(
            vec![p1.clone()], vec![], vec![p2_lead], vec![p2_ditto],
        );
        let state = MatchState::BattleState(battle);

        // P1 Splash; P2 switches to Ditto → Imposter transforms into Garchomp.
        let p1_cmd = PlayerCommand::Battle(simple_attack(Player::P1, vec![0]));
        let p2_cmd = PlayerCommand::Battle(vec![
            BattleCommand::Switch(SwitchCommand { party_index: 0 }),
        ]);
        let events = simulate_and_get_events(state, p1_cmd, p2_cmd);

        // The event stream must contain a Transformed announcing the copy.
        fn contains_transformed(evs: &[InformationEvent]) -> bool {
            use crate::information::information::EventKind;
            evs.iter().any(|e| {
                matches!(&e.kind, EventKind::Transformed { into_species, .. }
                    if *into_species == Species::Garchomp)
                    || contains_transformed(&e.reactions)
            })
        }
        assert!(contains_transformed(&events), "sim must emit Transformed for Imposter");

        let fog = fog_1v1(&p1, Species::Snorlax);
        let mut initial_fog = fog;
        initial_fog.p2_known_back_mons =
            vec![UnknownPokemonState::from_opponent_species(Species::Ditto, pd, 50)];

        let result = apply_information(
            UnknownMatchState::Battle(initial_fog),
            &events, false, pd, md, &HashMap::new(), &InferenceConfig::default(),
        );
        let result = match result {
            UnknownMatchState::Battle(b) => b,
            _ => panic!("expected Battle state"),
        };

        let t = &result.p2_active_mons[0];
        assert!(
            matches!(&t.possible_species, Unknown::Known(Species::Garchomp)),
            "transformed Ditto must display Garchomp; got {:?}", t.possible_species
        );
        assert!(t.pre_transform.is_some(), "pre_transform snapshot must be saved");
        // Copy source is our own Known mon → copied Atk is exact.
        assert_eq!(
            (t.min_stats[1], t.max_stats[1]), (true_atk, true_atk),
            "copied Atk must be exact (source is the observer's Known Garchomp)"
        );
        // Garchomp's move (Splash was P1's; the copy takes P1's move set).
        assert!(
            t.known_moves.contains(&Some(PokemonMove::Splash)),
            "transformed mon must copy the source's moves"
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

    // ── User report: BCP contradiction after a lead switch-out in doubles ────────
    //
    // Reported crash: lead Tyranitar + Lycanroc vs Charizard + Aerodactyl (P2's
    // belief), P1 switches Lycanroc (slot P1_1) out for Corviknight while every
    // other mon just Protects — `run_bcp` panics with "unsatisfiable clause (all
    // literals false)". Reported as intermittent, so this drives BOTH the
    // team-preview→battle transition (turn 0 — real lead sendout, so Tyranitar's
    // Sand Stream and its end-of-turn sand chip on Charizard are genuinely present,
    // not hand-waved away) and the switch turn (turn 1) through the REAL simulator
    // (`crate::simulator::simulate_turn`), branching over every outcome of both so
    // an RNG-dependent trigger can't hide in an unchecked branch. P2's fog belief is
    // built the same way the real server does (`session.rs::resolve_turn`'s
    // documented two-step flow, mirrored by `test_s37_*` above): team preview
    // (species-only for the non-viewer side) then replaying the transition's own
    // event log, extended here to doubles.
    #[test]
    fn test_lead_switchout_doubles_bcp_no_contradiction() {
        let pd = pokemon_dex();
        let md = move_dex();

        let p1_text = "\
Tyranitar @ Leftovers
Ability: Sand Stream
Level: 50
EVs: 252 HP / 252 Atk / 4 SpD
Adamant Nature
- Rock Slide
- Protect

Lycanroc @ Life Orb
Ability: Sand Rush
Level: 50
EVs: 252 Atk / 4 SpD / 252 Spe
Jolly Nature
- Accelerock
- Protect

Corviknight @ Leftovers
Ability: Pressure
Level: 50
EVs: 252 HP / 252 Def / 4 SpD
Impish Nature
- Brave Bird
- Protect
";
        let p2_text = "\
Charizard @ Choice Specs
Ability: Blaze
Level: 50
EVs: 4 Def / 252 SpA / 252 Spe
Timid Nature
- Flamethrower
- Protect

Aerodactyl @ Focus Sash
Ability: Pressure
Level: 50
EVs: 252 Atk / 4 SpD / 252 Spe
Jolly Nature
- Rock Slide
- Protect
";

        let preview = crate::simulator::team_preview_state_from_team_strings(
            p1_text, p2_text, pd, md, 2, 3, false,
        );

        let p1_tp_cmd = PlayerCommand::TeamPreview(crate::state::battle::TeamPreviewCommand {
            active_indices: vec![0, 1],
            back_indices: vec![2],
        });
        let p2_tp_cmd = PlayerCommand::TeamPreview(crate::state::battle::TeamPreviewCommand {
            active_indices: vec![0, 1],
            back_indices: vec![],
        });

        // ── Turn 0: real team-preview → battle transition. Per the engine's own
        // documented behaviour, team-preview resolution runs its own end-of-turn
        // before turn 1 — so Sand Stream's Sandstorm and its chip on Charizard are
        // already baked into these branches, not something this test has to fake.
        let turn0_branches = crate::simulator::simulate_turn(
            &MatchState::TeamPreviewState(preview.clone()), &p1_tp_cmd, &p2_tp_cmd,
            md, pd, false, 16, Some(Player::P2),
        );
        assert!(!turn0_branches.is_empty());

        let opponent_species: Vec<Species> =
            preview.p1_mons.iter().map(|m| m.species.clone()).collect();
        let my_team = preview.p2_mons.clone();

        let mut checked = 0;
        for (turn0_state, turn0_events_opt, _prob) in turn0_branches {
            if !matches!(&turn0_state, MatchState::BattleState(_)) {
                continue;
            }
            let Some(turn0_events) = turn0_events_opt else { continue };

            // P2's fog-of-war belief, seeded the same way `session.rs` does:
            // species-only team preview, then the real transition's own event log.
            let fog_preview = UnknownMatchState::team_preview_from_perspective(
                Player::P2, &my_team, &opponent_species, pd, 2, 3, 50,
            );
            let UnknownMatchState::TeamPreview(fog_preview) = fog_preview else {
                panic!("expected TeamPreview")
            };
            let fog = fog_preview.into_battle_state(Player::P2, &[], &[], &[0, 1], &[]);
            let fog_after_leads = apply_roundtrip(fog, turn0_events);

            // ── Turn 1 (the crash turn): P1 switches Lycanroc (slot 1) out for
            // Corviknight (party_index 0, its only bench mon); everyone else Protects.
            let p1_cmd = PlayerCommand::Battle(vec![
                BattleCommand::Attack(crate::state::battle::AttackCommand {
                    move_slot: 1, target: None, terastallize: false, mega_evolve: false,
                }),
                BattleCommand::Switch(SwitchCommand { party_index: 0 }),
            ]);
            let p2_cmd = PlayerCommand::Battle(simple_attack(Player::P2, vec![1, 1]));

            let turn1_branches = crate::simulator::simulate_turn(
                &turn0_state, &p1_cmd, &p2_cmd, md, pd, false, 16, Some(Player::P2),
            );
            for (_s, events_opt, _p) in turn1_branches {
                let Some(events) = events_opt else { continue };
                checked += 1;
                // Must not panic — this is the actual regression under test.
                let _ = apply_roundtrip(fog_after_leads.clone(), events);
            }
        }
        assert!(checked > 0, "expected at least one branch to be exercised");
    }

    // ── Live-triggered follow-up: switch-out + SAME-TURN Mega Evolution ──────────
    //
    // The plain switch-out above (no Mega Evolution) never crashed — the user's live
    // repro added one missing ingredient: P1 also Mega Evolves a DIFFERENT active mon
    // in the SAME turn as the switch. Real crash trace (P2's belief):
    //   turn=[Switch(P1_1 -> Sinistcha), MegaEvolution(P1_0 -> TyranitarMega),
    //         MoveUsed(P2_0, Protect), MoveUsed(P2_1, Protect), MoveUsed(P1_0, Protect),
    //         EndOfTurn]
    //   — unsatisfiable clause: [SpeedComparison { fast_idx: 1, slow_idx: 2, .. }]
    //   legend: 0:p1_active=TyranitarMega 1:p1_active=Sinistcha(NEW) 2:p2_active=Aerodactyl
    //           3:p2_active=Charizard 4..6:p1_possible_back(Corviknight,Raichu,Hydreigon)
    //           7..8:p2_known_back(Sylveon,Ariados)
    // Note p1_known_back is EMPTY — whatever occupied P1_1 before the switch (the mon
    // the surviving `fast_idx:1` clause is really about) isn't accounted for anywhere
    // in the post-switch roster, which is the real tell. Reproduces the same shape:
    // an earlier real turn establishes a persisted `SpeedComparison` for the P1_1
    // occupant (Lycanroc) against a P2 mon, then the crash turn switches Lycanroc out
    // for a bench mon WHILE Tyranitar (a different active slot) Mega Evolves.
    #[test]
    fn test_switchout_same_turn_mega_evolution_bcp_no_contradiction() {
        let pd = pokemon_dex();
        let md = move_dex();

        let p1_text = "\
Tyranitar @ Tyranitarite
Ability: Sand Stream
Level: 50
EVs: 252 HP / 252 Atk / 4 SpD
Adamant Nature
- Rock Slide
- Protect

Lycanroc @ Life Orb
Ability: Sand Rush
Level: 50
EVs: 252 Atk / 4 SpD / 252 Spe
Jolly Nature
- Accelerock
- Protect

Corviknight @ Leftovers
Ability: Pressure
Level: 50
EVs: 252 HP / 252 Def / 4 SpD
Impish Nature
- Brave Bird
- Protect
";
        let p2_text = "\
Charizard @ Choice Specs
Ability: Blaze
Level: 50
EVs: 4 Def / 252 SpA / 252 Spe
Timid Nature
- Flamethrower
- Protect

Aerodactyl @ Focus Sash
Ability: Pressure
Level: 50
EVs: 252 Atk / 4 SpD / 252 Spe
Jolly Nature
- Rock Slide
- Protect
";

        let preview = crate::simulator::team_preview_state_from_team_strings(
            p1_text, p2_text, pd, md, 2, 3, false,
        );

        let p1_tp_cmd = PlayerCommand::TeamPreview(crate::state::battle::TeamPreviewCommand {
            active_indices: vec![0, 1],
            back_indices: vec![2],
        });
        let p2_tp_cmd = PlayerCommand::TeamPreview(crate::state::battle::TeamPreviewCommand {
            active_indices: vec![0, 1],
            back_indices: vec![],
        });

        let turn0_branches = crate::simulator::simulate_turn(
            &MatchState::TeamPreviewState(preview.clone()), &p1_tp_cmd, &p2_tp_cmd,
            md, pd, false, 16, Some(Player::P2),
        );
        assert!(!turn0_branches.is_empty());

        let opponent_species: Vec<Species> =
            preview.p1_mons.iter().map(|m| m.species.clone()).collect();
        let my_team = preview.p2_mons.clone();

        let mut checked = 0;
        for (turn0_state, turn0_events_opt, _prob) in turn0_branches {
            if !matches!(&turn0_state, MatchState::BattleState(_)) {
                continue;
            }
            let Some(turn0_events) = turn0_events_opt else { continue };

            let fog_preview = UnknownMatchState::team_preview_from_perspective(
                Player::P2, &my_team, &opponent_species, pd, 2, 3, 50,
            );
            let UnknownMatchState::TeamPreview(fog_preview) = fog_preview else {
                panic!("expected TeamPreview")
            };
            let fog = fog_preview.into_battle_state(Player::P2, &[], &[], &[0, 1], &[]);
            let fog_after_leads = apply_roundtrip(fog, turn0_events);

            // ── Turn 1: everyone Protects — a real attack turn (no switch, no mega)
            // that lets Pass 4 observe move order among all four actives and (if the
            // resolved order makes it informative) persist a `SpeedComparison` tying
            // Lycanroc's (idx 1) hidden Spe bounds to a P2 mon's known Spe.
            let turn1_p1_cmd = PlayerCommand::Battle(vec![
                BattleCommand::Attack(crate::state::battle::AttackCommand {
                    move_slot: 1, target: None, terastallize: false, mega_evolve: false,
                }),
                BattleCommand::Attack(crate::state::battle::AttackCommand {
                    move_slot: 1, target: None, terastallize: false, mega_evolve: false,
                }),
            ]);
            let turn1_p2_cmd = PlayerCommand::Battle(simple_attack(Player::P2, vec![1, 1]));
            let turn1_branches = crate::simulator::simulate_turn(
                &turn0_state, &turn1_p1_cmd, &turn1_p2_cmd, md, pd, false, 16, Some(Player::P2),
            );

            for (turn1_state, turn1_events_opt, _p) in turn1_branches {
                if !matches!(&turn1_state, MatchState::BattleState(_)) {
                    continue;
                }
                let Some(turn1_events) = turn1_events_opt else { continue };
                let fog_after_turn1 = apply_roundtrip(fog_after_leads.clone(), turn1_events);

                // ── Turn 2 (the crash turn): P1 switches Lycanroc (slot 1) out for
                // Corviknight AND Mega Evolves Tyranitar (slot 0) in the SAME turn;
                // everyone else Protects — matching the live crash trace exactly.
                let turn2_p1_cmd = PlayerCommand::Battle(vec![
                    BattleCommand::Attack(crate::state::battle::AttackCommand {
                        move_slot: 1, target: None, terastallize: false, mega_evolve: true,
                    }),
                    BattleCommand::Switch(SwitchCommand { party_index: 0 }),
                ]);
                let turn2_p2_cmd = PlayerCommand::Battle(simple_attack(Player::P2, vec![1, 1]));
                let turn2_branches = crate::simulator::simulate_turn(
                    &turn1_state, &turn2_p1_cmd, &turn2_p2_cmd, md, pd, false, 16, Some(Player::P2),
                );
                for (_s2, turn2_events_opt, _p2) in turn2_branches {
                    let Some(turn2_events) = turn2_events_opt else { continue };
                    checked += 1;
                    // Must not panic — this is the actual regression under test.
                    let _ = apply_roundtrip(fog_after_turn1.clone(), turn2_events);
                }
            }
        }
        assert!(checked > 0, "expected at least one branch to be exercised");
    }

    // ── Corrected timing: the switch+mega turn is the VERY FIRST real turn ───────
    //
    // The user confirmed the crash turn had ZERO turns between the team-preview
    // lead-out and the switch+Mega turn — the test above inserted an extra combat
    // turn to *establish* the SpeedComparison first, which doesn't match. Since
    // `Statement::SpeedComparison` is constructed in exactly one place in the whole
    // engine (`pass4_speed_from_order`'s `windows(2)` loop, driven only by
    // `MoveUsed` events), and the outgoing P1_1 mon never has a `MoveUsed` event on
    // the crash turn itself, a stale clause referencing it is only possible if the
    // very first `apply_information` call (the team-preview transition) already
    // produces one — which requires investigating whether Sand Rush activating the
    // instant Sand Stream sets Sandstorm (both fire during this same lead-out) folds
    // into a same-turn Mega Evolution + switch. Real crash trace for context:
    //   P2 sent out Aerodactyl, P1 sent out Lycanroc, P2 sent out Charizard,
    //   P1 sent out Tyranitar, Aerodactyl's Unnerve!, Tyranitar's Sand Stream!
    //   The weather became Sandstorm! -- then immediately the crash turn:
    //   Switch(P1_1->Sinistcha), MegaEvolution(P1_0->TyranitarMega), 3xProtect, EndOfTurn.
    #[test]
    fn test_switchout_same_turn_mega_evolution_on_first_real_turn_no_contradiction() {
        let pd = pokemon_dex();
        let md = move_dex();

        let p1_text = "\
Tyranitar @ Tyranitarite
Ability: Sand Stream
Level: 50
EVs: 252 HP / 252 Atk / 4 SpD
Adamant Nature
- Rock Slide
- Protect

Lycanroc @ Life Orb
Ability: Sand Rush
Level: 50
EVs: 252 Atk / 4 SpD / 252 Spe
Jolly Nature
- Accelerock
- Protect

Corviknight @ Leftovers
Ability: Pressure
Level: 50
EVs: 252 HP / 252 Def / 4 SpD
Impish Nature
- Brave Bird
- Protect
";
        let p2_text = "\
Charizard @ Choice Specs
Ability: Blaze
Level: 50
EVs: 4 Def / 252 SpA / 252 Spe
Timid Nature
- Flamethrower
- Protect

Aerodactyl @ Focus Sash
Ability: Unnerve
Level: 50
EVs: 252 Atk / 4 SpD / 252 Spe
Jolly Nature
- Rock Slide
- Protect
";

        let preview = crate::simulator::team_preview_state_from_team_strings(
            p1_text, p2_text, pd, md, 2, 3, false,
        );

        let p1_tp_cmd = PlayerCommand::TeamPreview(crate::state::battle::TeamPreviewCommand {
            active_indices: vec![0, 1],
            back_indices: vec![2],
        });
        let p2_tp_cmd = PlayerCommand::TeamPreview(crate::state::battle::TeamPreviewCommand {
            active_indices: vec![0, 1],
            back_indices: vec![],
        });

        let turn0_branches = crate::simulator::simulate_turn(
            &MatchState::TeamPreviewState(preview.clone()), &p1_tp_cmd, &p2_tp_cmd,
            md, pd, false, 16, Some(Player::P2),
        );
        assert!(!turn0_branches.is_empty());

        let opponent_species: Vec<Species> =
            preview.p1_mons.iter().map(|m| m.species.clone()).collect();
        let my_team = preview.p2_mons.clone();

        let mut checked = 0;
        for (turn0_state, turn0_events_opt, _prob) in turn0_branches {
            if !matches!(&turn0_state, MatchState::BattleState(_)) {
                continue;
            }
            let Some(turn0_events) = turn0_events_opt else { continue };

            let fog_preview = UnknownMatchState::team_preview_from_perspective(
                Player::P2, &my_team, &opponent_species, pd, 2, 3, 50,
            );
            let UnknownMatchState::TeamPreview(fog_preview) = fog_preview else {
                panic!("expected TeamPreview")
            };
            let fog = fog_preview.into_battle_state(Player::P2, &[], &[], &[0, 1], &[]);
            let fog_after_leads = apply_roundtrip(fog, turn0_events);

            // ── The crash turn, run as the VERY FIRST real turn (no combat turn in
            // between): P1 switches Lycanroc (slot 1) out for Corviknight AND Mega
            // Evolves Tyranitar (slot 0) in the SAME turn; everyone else Protects.
            let turn1_p1_cmd = PlayerCommand::Battle(vec![
                BattleCommand::Attack(crate::state::battle::AttackCommand {
                    move_slot: 1, target: None, terastallize: false, mega_evolve: true,
                }),
                BattleCommand::Switch(SwitchCommand { party_index: 0 }),
            ]);
            let turn1_p2_cmd = PlayerCommand::Battle(simple_attack(Player::P2, vec![1, 1]));
            let turn1_branches = crate::simulator::simulate_turn(
                &turn0_state, &turn1_p1_cmd, &turn1_p2_cmd, md, pd, false, 16, Some(Player::P2),
            );
            for (_s, turn1_events_opt, _p) in turn1_branches {
                let Some(turn1_events) = turn1_events_opt else { continue };
                checked += 1;
                // Must not panic — this is the actual regression under test.
                let _ = apply_roundtrip(fog_after_leads.clone(), turn1_events);
            }
        }
        assert!(checked > 0, "expected at least one branch to be exercised");
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

    let events = vec![event(EventKind::Switch(SwitchState { disguise_species: None, max_hp: 0,
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
            SwitchState { disguise_species: None, max_hp: 0,
                slot:      p2(0),
                species:   Species::Garchomp,
                level:     50,
                hp:        PokemonHP::Percent(100),
                status:    None,
                tera_type: None,
            },
            SwitchState { disguise_species: None, max_hp: 0,
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

/// When Sandstorm naturally expires THIS end-of-turn (confirmed game behavior: the
/// turn weather "subsides," nobody takes residual chip that turn — `end_turn`
/// clears `state.weather` via `decrement_effect_timers` before `apply_pre_status_
/// residuals` runs), "no chip" carries zero information about item/ability and must
/// NOT emit a sand-immunity disjunction, even though `state.weather` (pass 1 hasn't
/// processed this EndOfTurn's `WeatherChanged` reaction yet) still reads Sandstorm.
/// Regression for the mega-stone/item Known-conflict this over-narrow produced
/// (e.g. `Known(SafetyGoggles)` forced on a mon whose true held item was later
/// revealed as its Mega Stone via `MegaEvolution`).
#[test]
fn test_sand_immunity_not_inferred_when_weather_expires_this_eot() {
    let mut p2_mon = unknown_mon();
    p2_mon.possible_types = Unknown::Known(vec![PokemonType::Normal]);
    p2_mon.possible_abilities = Unknown::Not(vec![Ability::AirLock, Ability::CloudNine]);

    let mut state = battle_with_p2(vec![p2_mon]);
    state.weather = Some(Weather::Sandstorm);

    // No DamageDealt (no visible chip) — but this EOT's own reactions show weather
    // naturally ending, fully explaining the absent chip.
    let eot = event_with(
        EventKind::EndOfTurn,
        vec![event(EventKind::WeatherChanged { weather: None })],
    );
    let result = apply(state, vec![eot]);

    let has_sand_immunity_clause = result.predicates.iter().any(|clause| {
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
        !has_sand_immunity_clause,
        "no sand-immunity clause should be emitted when Sandstorm naturally expires \
         this same end-of-turn — the absent chip is fully explained by expiry, not immunity"
    );
}

// ── Regression: S6 — EOT netting must not produce unsound heal/immunity clauses ──
//
// `emit_eot_hp_deltas` diffs HP across a whole EOT sub-phase with ONE before/after
// snapshot; a sand chip that triggers a pinch berry mid-chip (`deal_residual_damage`
// calls `take_damage`, which checks berries internally) can net into a `Healed`
// event, or — in the exact-cancel case — no HP-change event at all (just the
// berry's `ItemLost`). Both `pass_eot_heal` and `pass_eot_sand_immunity` must
// recognize this ambiguity and skip rather than draw an unsound conclusion.

/// A target that shows BOTH a `DamageDealt` and a `Healed` for itself in the same
/// EndOfTurn must not have a Leftovers/BlackSludge clause emitted for the Healed —
/// the co-occurring chip means the heal could be a chip-then-berry-overheal net
/// result, not a passive item.
#[test]
fn test_eot_heal_not_inferred_when_same_target_also_took_chip_this_eot() {
    let mut p2_mon = unknown_mon();
    p2_mon.item = Unknown::Not(vec![]);
    let state = battle_with_p2(vec![p2_mon]);

    let eot = event_with(
        EventKind::EndOfTurn,
        vec![
            event(EventKind::DamageDealt { max_hp: 0, target: p2(0), new_hp: PokemonHP::Percent(40) }),
            event(EventKind::Healed { max_hp: 0, target: p2(0), new_hp: PokemonHP::Percent(55) }),
        ],
    );
    let result = apply(state, vec![eot]);

    let has_leftovers_clause = result.predicates.iter().any(|c| {
        c.iter().any(|s| matches!(s, Statement::HasItem { item: Item::Leftovers, .. }))
    });
    assert!(
        !has_leftovers_clause,
        "no Leftovers clause should be emitted when the same target also took EOT \
         chip this turn (chip/berry netting ambiguity)"
    );
}

/// A target that shows no sand chip but DOES show a `Healed` event this same EOT
/// must not have a sand-immunity clause emitted — the "no chip" observation could
/// be explained by the chip being clobbered by a pinch berry, not immunity.
#[test]
fn test_sand_immunity_not_inferred_when_target_healed_this_eot() {
    let mut p2_mon = unknown_mon();
    p2_mon.possible_types = Unknown::Known(vec![PokemonType::Normal]);
    p2_mon.possible_abilities = Unknown::Not(vec![Ability::AirLock, Ability::CloudNine]);

    let mut state = battle_with_p2(vec![p2_mon]);
    state.weather = Some(Weather::Sandstorm);

    // No DamageDealt (no visible chip), but a Healed for the same mon this EOT —
    // the netted result of a sand chip clobbered by a pinch berry.
    let eot = event_with(
        EventKind::EndOfTurn,
        vec![event(EventKind::Healed { max_hp: 0, target: p2(0), new_hp: PokemonHP::Percent(55) })],
    );
    let result = apply(state, vec![eot]);

    let has_sand_immunity_clause = result.predicates.iter().any(|clause| {
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
        !has_sand_immunity_clause,
        "no sand-immunity clause should be emitted when the same mon was healed \
         this EOT (chip/berry netting ambiguity)"
    );
}

/// A target that shows no sand chip and no Healed, but DID consume a berry this EOT
/// (the exact-cancel case: chip damage and berry heal net to precisely zero, so
/// neither DamageDealt nor Healed appears) must also not have a sand-immunity
/// clause emitted.
#[test]
fn test_sand_immunity_not_inferred_when_target_ate_berry_this_eot_exact_cancel() {
    let mut p2_mon = unknown_mon();
    p2_mon.possible_types = Unknown::Known(vec![PokemonType::Normal]);
    p2_mon.possible_abilities = Unknown::Not(vec![Ability::AirLock, Ability::CloudNine]);

    let mut state = battle_with_p2(vec![p2_mon]);
    state.weather = Some(Weather::Sandstorm);

    // No DamageDealt, no Healed — but ItemLost for a consumed berry is present,
    // proving the chip fired and was exactly offset.
    let eot = event_with(
        EventKind::EndOfTurn,
        vec![event(EventKind::ItemLost {
            slot: p2(0),
            item: Item::SitrusBerry,
            consumed: true,
        })],
    );
    let result = apply(state, vec![eot]);

    let has_sand_immunity_clause = result.predicates.iter().any(|clause| {
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
        !has_sand_immunity_clause,
        "no sand-immunity clause should be emitted when the same mon consumed a \
         berry this EOT, even with no visible HP-change event (exact-cancel case)"
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
            vec![event(EventKind::DamageDealt { max_hp: 0, target: p2(0), new_hp: PokemonHP::Percent(75) })],
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
    // Mold Breaker / Turboblaze / Teravolt must also be excluded — any of those would
    // silently suppress the defender's Rough Skin / Iron Barbs (both are on the
    // canonical `ability_is_ignorable` list) with no reveal, so their possibility alone
    // (sound: might be true) blocks the exclusion below just like Magic Guard does.
    // NeutralizingGas must be excluded on BOTH actives, or the defender-suppression
    // gate (sound: the chip could be suppressed field-wide) skips the exclusions.
    let mut p1_mon = UnknownPokemonState::from_opponent_species(Species::Garchomp, &HashMap::new(), 50);
    p1_mon.item = Unknown::Known(Item::None);
    p1_mon.possible_abilities = Unknown::Not(vec![
        Ability::LongReach,
        Ability::MagicGuard,
        Ability::NeutralizingGas,
        Ability::MoldBreaker,
        Ability::Turboblaze,
        Ability::Teravolt,
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
            vec![event(EventKind::DamageDealt { max_hp: 0, target: p2(0), new_hp: PokemonHP::Percent(75) })],
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

    // Defender: abilities unknown except Klutz — Neutralizing Gas remains possible
    // (the suppression gate under test), while the S21 item-inertness gate is
    // satisfied so the Helmet exclusion below can still fire.
    let mut p2_mon = unknown_mon();
    p2_mon.possible_abilities = Unknown::Not(vec![Ability::Klutz]);
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
            vec![event(EventKind::DamageDealt { max_hp: 0, target: p2(0), new_hp: PokemonHP::Percent(75) })],
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
        switches: vec![SwitchState { disguise_species: None, max_hp: 0,
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

    let switch_ev = event(EventKind::Switch(SwitchState { disguise_species: None, max_hp: 0,
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
    let t1 = vec![event(EventKind::Switch(SwitchState { disguise_species: None, max_hp: 0,
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
    let t2 = vec![event(EventKind::Switch(SwitchState { disguise_species: None, max_hp: 0,
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

    let t1 = vec![event(EventKind::Switch(SwitchState { disguise_species: None, max_hp: 0,
        slot: p2(0),
        species: Species::Garchomp,
        level: 50,
        hp: PokemonHP::Percent(100),
        status: None,
        tera_type: None,
    }))];
    let after_t1 = apply(state, t1);

    // Snorlax returns at 40% — same as it left (no Regenerator gain).
    let t2 = vec![event(EventKind::Switch(SwitchState { disguise_species: None, max_hp: 0,
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

    let t1 = vec![event(EventKind::Switch(SwitchState { disguise_species: None, max_hp: 0,
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
    let t2 = vec![event(EventKind::Switch(SwitchState { disguise_species: None, max_hp: 0,
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

    let t1 = vec![event(EventKind::Switch(SwitchState { disguise_species: None, max_hp: 0,
        slot: p2(0),
        species: Species::Garchomp,
        level: 50,
        hp: PokemonHP::Percent(100),
        status: None,
        tera_type: None,
    }))];
    let after_t1 = apply(state, t1);

    // Returns at 100% — could be 70% + 33% Regen (capped) or just full HP.
    let t2 = vec![event(EventKind::Switch(SwitchState { disguise_species: None, max_hp: 0,
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
    let t1 = vec![event(EventKind::Switch(SwitchState { disguise_species: None, max_hp: 0,
        slot: p2(0), species: Species::Garchomp, level: 50,
        hp: PokemonHP::Percent(100), status: None, tera_type: None,
    }))];
    let after_t1 = apply(state, t1);

    // Turn 2: Snorlax returns at 83% → Regenerator inferred.
    let t2 = vec![event(EventKind::Switch(SwitchState { disguise_species: None, max_hp: 0,
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
    let t3 = vec![event(EventKind::Switch(SwitchState { disguise_species: None, max_hp: 0,
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
        vec![event(EventKind::Healed { max_hp: 0, target: p2(0), new_hp: PokemonHP::Percent(90) })],
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
        vec![event(EventKind::Healed { max_hp: 0, target: p2(0), new_hp: PokemonHP::Percent(90) })],
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

    let sw = event(EventKind::Switch(SwitchState { disguise_species: None, max_hp: 0,
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

    let sw = event(EventKind::Switch(SwitchState { disguise_species: None, max_hp: 0,
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
        EventKind::Switch(SwitchState { disguise_species: None, max_hp: 0,
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

    let sw = event(EventKind::Switch(SwitchState { disguise_species: None, max_hp: 0,
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

    let sw = event(EventKind::Switch(SwitchState { disguise_species: None, max_hp: 0,
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

// ── Regression: S1 — P2's active mon_idx must survive P1-side bench churn ────────
//
// Under the pre-S1 layout (`[p1_active, p1_known_back, p1_possible_back, p2_active,
// ...]`), P2's active mon_idx was `p1_active.len() + p1_known_back.len() +
// p1_possible_back.len()` — it depended on P1's CURRENT bench size. Any P1-side
// switch (which pushes the outgoing mon onto p1_known_back/p1_possible_back, then
// removes the incoming mon from wherever it was benched) shifted every mon_idx that
// came after P1's bench segments, silently retargeting persistent `Statement`s
// (SpeedComparison, weather/terrain/screen setters, HasItem/HasAbility clauses) that
// reference a P2 mon onto a DIFFERENT physical Pokémon — purely from P1 activity
// that P2 was not even involved in.
//
// The S1 fix places both active segments first (`[p1_active, p2_active,
// p1_known_back, ...]`), so P2's active mon_idx depends only on
// `p1_active_mons.len()`, which is fixed for the whole battle after the initial
// lead-sending bootstrap. This test grows P1's known-back roster from 0 to 2 mons
// via real switches and asserts a clause captured against P2's active mon BEFORE
// those switches still resolves to the SAME P2 mon (by species) afterward.

#[test]
fn test_p2_active_mon_idx_stable_across_p1_bench_churn() {
    let p1_mon_a = unknown_mon_species(Species::Garchomp);
    let p1_mon_c = unknown_mon_species(Species::Corviknight);
    let p2_mon = unknown_mon_species(Species::Snorlax);

    let mut state = battle_1v1(p1_mon_a, p2_mon);
    // P1 already has one known-back mon before we start — this is exactly the
    // condition that made the OLD layout diverge from the NEW one (P2's mon_idx
    // was 2 under the old layout here, 1 under the new one).
    state.p1_known_back_mons = vec![p1_mon_c];

    // Capture P2's active mon_idx and manually install a persistent clause
    // referencing it, mirroring what Pass 2/3/4 would emit in practice.
    let p2_idx_before = mon_idx_for_active_slot(&state, &p2(0)).unwrap();
    state.predicates.push(vec![Statement::HasItem {
        mon_idx: p2_idx_before,
        item: Item::Leftovers,
    }]);

    // P1 switches Garchomp out for Corviknight (already on the bench), then
    // switches Corviknight out for a brand-new mon (Snorlax) never seen before —
    // this exercises both a Vec::remove (shrinking p1_known_back) and a push
    // (growing it), the two operations that caused the old layout to drift.
    let switch_in_corviknight = event(EventKind::Switch(SwitchState { disguise_species: None, max_hp: 0,
        slot: p1(0),
        species: Species::Corviknight,
        level: 50,
        hp: PokemonHP::Percent(100),
        status: None,
        tera_type: None,
    }));
    let after_first_switch = apply(state, vec![switch_in_corviknight]);

    let switch_in_new_mon = event(EventKind::Switch(SwitchState { disguise_species: None, max_hp: 0,
        slot: p1(0),
        species: Species::Alakazam,
        level: 50,
        hp: PokemonHP::Percent(100),
        status: None,
        tera_type: None,
    }));
    let result = apply(after_first_switch, vec![switch_in_new_mon]);

    // P2's active mon_idx (recomputed fresh, post-churn) must still be the SAME
    // integer as before — proving it never depended on P1's bench size.
    let p2_idx_after = mon_idx_for_active_slot(&result, &p2(0)).unwrap();
    assert_eq!(
        p2_idx_before, p2_idx_after,
        "P2's active mon_idx must not shift due to P1-side bench churn"
    );

    // And the mon actually AT that index must still be Snorlax (P2's real mon),
    // never one of the P1 mons that churned through the bench.
    let mon_at_idx = get_mon_by_idx(&result, p2_idx_before).unwrap();
    assert!(
        matches!(&mon_at_idx.possible_species, Unknown::Known(s) if *s == Species::Snorlax),
        "mon_idx {p2_idx_before} must still resolve to P2's Snorlax, got {:?}",
        mon_at_idx.possible_species
    );

    // The clause captured before the churn is a unit clause, so Pass 6 (BCP)
    // force-resolves it immediately into the mon's item field (and removes it from
    // `predicates`). The pre-fix bug would have forced this onto whichever P1 mon
    // ended up at the stale index instead — assert it landed on P2's Snorlax.
    assert_eq!(
        mon_at_idx.item,
        Unknown::Known(Item::Leftovers),
        "the pre-churn clause must have resolved onto P2's Snorlax, not a P1 mon"
    );
}

// ── Regression: SimultaneousSwitch send-out order must not corrupt active-slot
// placement ───────────────────────────────────────────────────────────────────
//
// The concrete simulator emits the initial lead send-out `SimultaneousSwitch` in
// GLOBAL cross-side effective-speed order (`process_sendouts_in_speed_order_branching`,
// simulator/mod.rs) — NOT slot-index order. A side's own faster lead can appear in
// the `switches` list before its slower teammate even though it occupies a HIGHER
// slot_index. `pass1_switch`'s active-slot placement (`if slot_i < actives.len()
// {overwrite} else {push}`) silently assumed ascending slot-index processing per
// side: processing the higher slot first pushed it to array position 0 (since the
// Vec was still empty), and the lower slot processed next then overwrote that
// position — silently destroying the first mon with no bench record at all. This
// left `p1_active_mons` permanently one entry short, desyncing every P2 `mon_idx`
// computation (`p1_active_mons.len() + slot_i`) until something eventually grew
// the Vec back — the real-world trigger for a live BCP soundness panic (a
// `SpeedComparison` clause baked in against the wrong, since-shifted index).
// Reproduces the live crash's exact shape: P1's slot-1 lead (Lycanroc) listed
// BEFORE its slot-0 lead (Tyranitar) in the SimultaneousSwitch.
#[test]
fn test_simultaneous_switch_placement_survives_out_of_slot_order() {
    let p2_team = vec![
        crate::state::pokemon::build_pokemon_state(
            Species::Charizard, &HashMap::new(), &HashMap::new(), Some(50),
            None, None, None, None, None, None, None, None, true,
        ),
        crate::state::pokemon::build_pokemon_state(
            Species::Aerodactyl, &HashMap::new(), &HashMap::new(), Some(50),
            None, None, None, None, None, None, None, None, true,
        ),
    ];
    let opponent_species = vec![Species::Tyranitar, Species::Lycanroc];

    let preview = UnknownMatchState::team_preview_from_perspective(
        Player::P2, &p2_team, &opponent_species, &HashMap::new(), 2, 2, 50,
    );
    let UnknownMatchState::TeamPreview(preview) = preview else {
        panic!("expected TeamPreview")
    };
    let fog = preview.into_battle_state(Player::P2, &[], &[], &[0, 1], &[]);

    // The bug-triggering order: P1's slot-1 lead (faster, Lycanroc) listed BEFORE
    // its slot-0 lead (slower, Tyranitar) — exactly what the concrete simulator's
    // global speed sort produces when the higher-slot mon is faster.
    let events = vec![event(EventKind::SimultaneousSwitch {
        switches: vec![
            SwitchState { disguise_species: None, max_hp: 0,
                slot: p1(1), species: Species::Lycanroc, level: 50,
                hp: PokemonHP::Percent(100), status: None, tera_type: None,
            },
            SwitchState { disguise_species: None, max_hp: 0,
                slot: p1(0), species: Species::Tyranitar, level: 50,
                hp: PokemonHP::Percent(100), status: None, tera_type: None,
            },
        ],
    })];

    let result = apply_information(
        UnknownMatchState::Battle(fog), &events, true, &HashMap::new(), &HashMap::new(),
        &HashMap::new(), &InferenceConfig::default(),
    );
    let UnknownMatchState::Battle(b) = result else {
        panic!("expected Battle state")
    };

    assert_eq!(
        b.p1_active_mons.len(), 2,
        "both P1 leads must survive the transition regardless of send-out order; got {:?}",
        b.p1_active_mons.iter().map(|m| &m.possible_species).collect::<Vec<_>>()
    );
    assert!(
        matches!(&b.p1_active_mons[0].possible_species, Unknown::Known(s) if *s == Species::Tyranitar),
        "slot 0 must be Tyranitar; got {:?}", b.p1_active_mons[0].possible_species
    );
    assert!(
        matches!(&b.p1_active_mons[1].possible_species, Unknown::Known(s) if *s == Species::Lycanroc),
        "slot 1 must be Lycanroc, not silently destroyed by the out-of-order switch; got {:?}",
        b.p1_active_mons[1].possible_species
    );
}

// ── Live-crash isolation: does the switch purge actually drop a stale         ────
// SpeedComparison tied to the outgoing mon's mon_idx, with a same-turn Mega Evolution
// on a DIFFERENT slot in the mix? Direct construction from the live crash's own
// mon_idx legend (0:Tyranitar 1:X(->Sinistcha) 2:Aerodactyl 3:Charizard
// 4-6:p1_possible_back 7-8:p2_known_back), skipping the real simulator so the purge
// mechanics can be checked in total isolation.
#[test]
fn test_switch_purges_stale_speed_comparison_with_same_turn_mega_evolution() {
    let mut state = battle_nvn(
        vec![unknown_mon_species(Species::Tyranitar), unknown_mon_species(Species::Lycanroc)],
        vec![unknown_mon_species(Species::Aerodactyl), unknown_mon_species(Species::Charizard)],
    );
    state.p1_possible_back_mons = vec![
        unknown_mon_species(Species::Sinistcha),
        unknown_mon_species(Species::Corviknight),
        unknown_mon_species(Species::Raichu),
        unknown_mon_species(Species::Hydreigon),
    ];
    state.p2_known_back_mons =
        vec![unknown_mon_species(Species::Sylveon), unknown_mon_species(Species::Ariados)];

    // Inject the exact stale clause from the live crash: idx 1 is the mon about to
    // leave P1_1 (Lycanroc here), idx 2 is Aerodactyl.
    state.predicates = vec![vec![Statement::SpeedComparison {
        fast_idx: 1,
        slow_idx: 2,
        fast_mult: 4,
        slow_mult: 4,
    }]];

    let events = vec![
        event(EventKind::Switch(SwitchState {
            disguise_species: None, max_hp: 0,
            slot: p1(1), species: Species::Sinistcha, level: 50,
            hp: PokemonHP::Percent(100), status: None, tera_type: None,
        })),
        event(EventKind::MegaEvolution { slot: p1(0), into: Species::TyranitarMega }),
        event(EventKind::MoveUsed { user: p2(0), move_used: PokemonMove::Protect, targets: vec![] }),
        event(EventKind::MoveUsed { user: p2(1), move_used: PokemonMove::Protect, targets: vec![] }),
        event(EventKind::MoveUsed { user: p1(0), move_used: PokemonMove::Protect, targets: vec![] }),
        event(EventKind::EndOfTurn),
    ];
    let result = apply(state, events);

    assert!(
        result.predicates.iter().all(|clause| {
            !clause.iter().any(
                |lit| matches!(lit, Statement::SpeedComparison { fast_idx, .. } if *fast_idx == 1),
            )
        }),
        "stale SpeedComparison{{fast_idx:1}} clause survived the P1_1 switch \
         (should have been purged by `purge_mon_scoped_knowledge`): {:?}",
        result.predicates
    );

    // The outgoing mon (Lycanroc) must be preserved on the bench, not silently lost —
    // the live crash's mon_idx legend showed an empty `p1_known_back`, which would
    // mean the outgoing mon vanished from the roster entirely instead of landing here.
    assert!(
        result.p1_known_back_mons.iter().any(
            |m| matches!(&m.possible_species, Unknown::Known(s) if *s == Species::Lycanroc),
        ),
        "outgoing Lycanroc must be preserved in p1_known_back_mons after the switch; got {:?}",
        result.p1_known_back_mons.iter().map(|m| &m.possible_species).collect::<Vec<_>>()
    );
}

// ── Information modes: describe.rs rendering, open-sheet reveal, team-preview →
// battle conversion (bring-N-of-M known/possible-back split) ─────────────────────
mod information_mode_tests {
    use super::*;
    use crate::information::describe::{
        describe_clause, describe_move_slot, describe_statement, describe_unknown,
        describe_unknown_item,
    };
    use crate::information::unknowns::{InformationMode, UnknownTeamPreviewState};
    use crate::state::pokemon::{build_pokemon_state, Nature, PokemonState};

    // ── describe.rs ───────────────────────────────────────────────────────────

    #[test]
    fn test_describe_unknown_known_possibly_not() {
        assert_eq!(describe_unknown(&Unknown::Known(Ability::RoughSkin)), "Rough Skin");
        assert_eq!(
            describe_unknown(&Unknown::Possibly(vec![Ability::RoughSkin, Ability::SandVeil])),
            "Rough Skin or Sand Veil"
        );
        assert_eq!(describe_unknown(&Unknown::<Ability>::Not(vec![])), "Unknown");
    }

    #[test]
    fn test_describe_unknown_item_shorter_list_uses_whitelist() {
        let pool: HashSet<Item> =
            [Item::Leftovers, Item::BlackSludge, Item::ChoiceBand, Item::ChoiceSpecs]
                .into_iter()
                .collect();
        // Excluding 1 of 4 leaves 3 possible — the exclusion phrasing (1 item) is
        // shorter than the possible-list phrasing (3 items).
        let excl = Unknown::Not(vec![Item::ChoiceBand]);
        assert_eq!(describe_unknown_item(&excl, Some(&pool)), "not Choice Band");
    }

    #[test]
    fn test_describe_unknown_item_possibly_lists_candidates() {
        let u = Unknown::Possibly(vec![Item::Leftovers, Item::BlackSludge]);
        assert_eq!(describe_unknown_item(&u, None), "Leftovers or Black Sludge");
    }

    #[test]
    fn test_describe_unknown_item_no_whitelist_renders_exclusions() {
        // Without a whitelist (~1,000 items technically possible), a short exclusion
        // list is virtually always the shorter description.
        let u = Unknown::Not(vec![Item::ChoiceBand, Item::ChoiceSpecs]);
        assert_eq!(describe_unknown_item(&u, None), "not Choice Band, not Choice Specs");
    }

    #[test]
    fn test_describe_statement_has_item_resolves_mon_label() {
        let state =
            battle_1v1(unknown_mon_species(Species::Snorlax), unknown_mon_species(Species::Garchomp));
        // mon_idx 0 = p1_active[0] (Snorlax), per the actives-first flat ordering.
        let stmt = Statement::HasItem { mon_idx: 0, item: Item::Leftovers };
        assert_eq!(describe_statement(&stmt, &state), "Snorlax's item is Leftovers");
    }

    #[test]
    fn test_describe_clause_joins_with_or() {
        let state =
            battle_1v1(unknown_mon_species(Species::Snorlax), unknown_mon_species(Species::Garchomp));
        let clause = vec![
            Statement::HasItem { mon_idx: 0, item: Item::Leftovers },
            Statement::HasItem { mon_idx: 0, item: Item::BlackSludge },
        ];
        assert_eq!(
            describe_clause(&clause, &state),
            "Snorlax's item is Leftovers OR Snorlax's item is Black Sludge"
        );
    }

    #[test]
    fn test_describe_move_slot_unknown_shows_placeholder() {
        assert_eq!(describe_move_slot(Some(PokemonMove::Tackle)), "Tackle");
        assert_eq!(describe_move_slot(None), "???");
    }

    // ── from_opponent_open_sheet ──────────────────────────────────────────────

    fn concrete_mon(
        species: Species,
        ability: Ability,
        item: Item,
        nature: Nature,
        moves: [Option<PokemonMove>; 4],
    ) -> PokemonState {
        build_pokemon_state(
            species, &HashMap::new(), &HashMap::new(), Some(50), Some(moves),
            None, Some(ability), Some(nature), Some(item), None, None, None, false,
        )
    }

    #[test]
    fn test_open_sheet_reveals_moves_item_ability_but_not_nature_or_evs() {
        let mon = concrete_mon(
            Species::Garchomp, Ability::RoughSkin, Item::ChoiceScarf, Nature::Jolly,
            [Some(PokemonMove::Earthquake), Some(PokemonMove::Outrage), None, None],
        );
        let unk = UnknownPokemonState::from_opponent_open_sheet(&mon, &HashMap::new(), 50, false, false);

        assert_eq!(unk.item, Unknown::Known(Item::ChoiceScarf));
        assert_eq!(unk.possible_abilities, Unknown::Known(Ability::RoughSkin));
        assert_eq!(
            unk.known_moves,
            [Some(PokemonMove::Earthquake), Some(PokemonMove::Outrage), None, None]
        );
        // Nature/EVs are NEVER on a sheet — must stay fully unknown / worst-case bounded.
        assert_eq!(unk.possible_natures, Unknown::Not(vec![]));
        assert_eq!(unk.min_evs, [0; 6]);
        assert_eq!(unk.max_evs, [252; 6]);
    }

    #[test]
    fn test_open_sheet_natures_tightens_stat_bounds_using_the_real_nature() {
        let mon = concrete_mon(
            Species::Garchomp, Ability::RoughSkin, Item::ChoiceScarf, Nature::Jolly,
            [Some(PokemonMove::Earthquake), None, None, None],
        );
        let pre_reveal = UnknownPokemonState::from_opponent_species(Species::Garchomp, &HashMap::new(), 50);
        let unk = UnknownPokemonState::from_opponent_open_sheet(&mon, &HashMap::new(), 50, true, false);

        assert_eq!(unk.possible_natures, Unknown::Known(Nature::Jolly));
        // Jolly NERFS SpA (fixed 0.9 mod now that nature is known, not the independent
        // 1.1 "best case" a fully-unknown nature assumes) — revealing it must strictly
        // lower the SpA ceiling.
        assert!(
            unk.max_stats[3] < pre_reveal.max_stats[3],
            "revealing a SpA-nerfing nature must lower the SpA ceiling: {} !< {}",
            unk.max_stats[3], pre_reveal.max_stats[3]
        );
        // Jolly BOOSTS Speed (fixed 1.1 mod, not the independent 0.9 "worst case") —
        // revealing it must strictly raise the Speed floor.
        assert!(
            unk.min_stats[5] > pre_reveal.min_stats[5],
            "revealing a Speed-boosting nature must raise the Speed floor: {} !> {}",
            unk.min_stats[5], pre_reveal.min_stats[5]
        );
    }

    // ── into_battle_state: opponent bench starts entirely unrevealed ──────────

    #[test]
    fn test_into_battle_state_p2_bench_starts_possible_not_known() {
        // 6 species shown at preview; P2 leads with index 0, the rest (1..5) are
        // reserve — but NONE of the reserve has actually been sent to the field yet,
        // and (post-D3 fix) neither has the "lead" itself, from the belief's point of
        // view: `into_battle_state` alone no longer places ANY P2 mon active — see its
        // doc comment. The belief only learns who's really active once the caller
        // (`session.rs::resolve_turn`) runs `apply_information` over the transition's
        // own event log afterward (covered by `test_into_battle_state_then_apply_information_places_lead_active`
        // below). `known_back` must only ever hold battle-confirmed
        // (revealed-then-withdrawn) mons (see `bench_outgoing_mon` / `pass1_switch`'s
        // known-then-possible fallback), so every P2 mon — active, brought-but-not-
        // leading, and never-brought alike — starts in `possible_back`, not
        // `known_back`. Regression for the TODO.md fix: opponent back mons were
        // "immediately showing up" as already-revealed at turn 0.
        let p2_mons: Vec<UnknownPokemonState> = [
            Species::Garchomp, Species::Snorlax, Species::Corviknight,
            Species::Charizard, Species::Ferrothorn, Species::Toxapex,
        ]
        .into_iter()
        .map(unknown_mon_species)
        .collect();
        let preview = UnknownTeamPreviewState {
            active_per_side: 1,
            brought_per_side: 4,
            p1_mons: vec![unknown_mon_species(Species::Snorlax)],
            p2_mons,
        };

        let battle = preview.into_battle_state(Player::P1, &[0], &[], &[0], &[1, 2]);

        assert!(
            battle.p2_active_mons.is_empty(),
            "into_battle_state alone must not place any P2 mon active; got {:?}",
            battle.p2_active_mons.iter().map(|m| &m.possible_species).collect::<Vec<_>>()
        );
        // Nothing is battle-confirmed yet — known_back starts empty regardless of
        // which indices were passed as "back" vs left off entirely.
        assert!(battle.p2_known_back_mons.is_empty());
        // All 6 P2 mons (including the eventual lead, Garchomp) land in
        // possible_back — the belief has no ground-truth concept of "active" yet.
        assert_eq!(battle.p2_possible_back_mons.len(), 6);
        let possible_species: HashSet<Species> = battle
            .p2_possible_back_mons
            .iter()
            .filter_map(|m| match &m.possible_species {
                Unknown::Known(s) => Some(s.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            possible_species,
            [
                Species::Garchomp, Species::Snorlax, Species::Corviknight,
                Species::Charizard, Species::Ferrothorn, Species::Toxapex,
            ]
            .into_iter()
            .collect()
        );
        // The viewer's own side is unaffected — P1 leads/back mons still populate
        // directly (never masked for display) and never gain a "possible back" gap.
        assert_eq!(battle.p1_active_mons.len(), 1);
        assert_eq!(battle.p1_possible_back_mons.len(), 0);
    }

    /// Companion to the test above: the intended two-step flow (`into_battle_state`
    /// seeds, then `apply_information` walks the transition's own `SimultaneousSwitch`
    /// event) must reproduce the OLD single-step behavior exactly for a normal,
    /// non-disguised lead — same species, same open-sheet fields, and the lead is
    /// removed from `possible_back` (not left duplicated there).
    #[test]
    fn test_into_battle_state_then_apply_information_places_lead_active() {
        let p2_mons: Vec<UnknownPokemonState> = [
            Species::Garchomp, Species::Snorlax, Species::Corviknight,
        ]
        .into_iter()
        .map(unknown_mon_species)
        .collect();
        let preview = UnknownTeamPreviewState {
            active_per_side: 1,
            brought_per_side: 3,
            p1_mons: vec![unknown_mon_species(Species::Snorlax)],
            p2_mons,
        };
        let seeded = preview.into_battle_state(Player::P1, &[0], &[], &[0], &[1, 2]);

        let switch_events = vec![event(EventKind::SimultaneousSwitch {
            switches: vec![
                SwitchState { disguise_species: None, max_hp: 0,
                    slot: p1(0), species: Species::Snorlax, level: 50,
                    hp: PokemonHP::Number(200), status: None, tera_type: None,
                },
                SwitchState { disguise_species: None, max_hp: 0,
                    slot: p2(0), species: Species::Garchomp, level: 50,
                    hp: PokemonHP::Percent(100), status: None, tera_type: None,
                },
            ],
        })];
        let result = apply_ex(seeded, switch_events, HashMap::new(), HashMap::new());

        assert_eq!(result.p2_active_mons.len(), 1);
        assert!(
            matches!(&result.p2_active_mons[0].possible_species, Unknown::Known(s) if *s == Species::Garchomp),
            "the lead must resolve to Known(Garchomp), got {:?}",
            result.p2_active_mons[0].possible_species
        );
        // Garchomp must be PULLED from possible_back, not left duplicated there.
        assert_eq!(result.p2_possible_back_mons.len(), 2);
        assert!(
            !result.p2_possible_back_mons.iter().any(|m|
                matches!(&m.possible_species, Unknown::Known(Species::Garchomp))),
            "the now-active Garchomp must not remain in possible_back"
        );
    }

    /// The actual TODO.md regression, under the parallel-hypothesis model: a leading
    /// Pokémon whose DISPLAYED species could be a Zoroark disguise (a Zoroark forme
    /// sits elsewhere in the team-preview roster, not selected as this lead) must end
    /// up carrying a live `possible_illusion_state` — `possible_species` itself stays
    /// pinned to `Known`, per the new architecture. The benched Zoroark roster entry
    /// must also survive untouched in `possible_back` — "Zoroark should show up as
    /// possibly in the back when led."
    #[test]
    fn test_zoroark_possibly_in_back_from_team_preview() {
        let p2_mons: Vec<UnknownPokemonState> = [
            Species::Milotic, Species::Zoroark, Species::Corviknight,
        ]
        .into_iter()
        .map(unknown_mon_species)
        .collect();
        let preview = UnknownTeamPreviewState {
            active_per_side: 1,
            brought_per_side: 3,
            p1_mons: vec![unknown_mon_species(Species::Snorlax)],
            p2_mons,
        };
        // P2's TRUE physical lead is index 1 (Zoroark), but the event stream carries
        // the DISPLAYED species (Milotic) — exactly what
        // `battle_state_from_preview_branching`'s perspective-gated `species` field
        // would produce for the observer. `into_battle_state` seeds every eligible
        // roster entry with a Zoroark hypothesis up front (`seed_illusion_hypotheses`).
        let seeded = preview.into_battle_state(Player::P1, &[0], &[], &[1], &[0, 2]);
        assert_eq!(
            seeded.p2_unresolved_zoroark_count, 1,
            "team preview must detect the one real Zoroark on P2's roster"
        );

        let switch_events = vec![event(EventKind::SimultaneousSwitch {
            switches: vec![
                SwitchState { disguise_species: None, max_hp: 0,
                    slot: p1(0), species: Species::Snorlax, level: 50,
                    hp: PokemonHP::Number(200), status: None, tera_type: None,
                },
                SwitchState { disguise_species: None, max_hp: 0,
                    slot: p2(0), species: Species::Milotic, level: 50,
                    hp: PokemonHP::Percent(100), status: None, tera_type: None,
                },
            ],
        })];
        let result = apply_ex(seeded, switch_events, HashMap::new(), HashMap::new());

        assert_eq!(
            result.p2_active_mons[0].possible_species,
            Unknown::Known(Species::Milotic),
            "species stays pinned to the shown species, never Possibly"
        );
        assert!(
            result.p2_active_mons[0].possible_illusion_state.is_some(),
            "a leading disguise-eligible mon must carry a live Zoroark hypothesis"
        );
        // Zoroark's own roster entry must still be present ("possibly in the back"),
        // not consumed by the active-slot's hypothesis attachment.
        assert!(
            result.p2_possible_back_mons.iter().any(|m|
                matches!(&m.possible_species, Unknown::Known(Species::Zoroark))),
            "Zoroark's own roster entry must remain in possible_back; possible_back = {:?}",
            result.p2_possible_back_mons.iter().map(|m| &m.possible_species).collect::<Vec<_>>()
        );
    }

    /// Doubles-only: if the same species ends up shown on two active slots at once,
    /// Species Clause guarantees exactly one is real — the other must be this side's
    /// Illusion forme in disguise. The newly-arrived duplicate slot must pick up a
    /// hypothesis (sound, if imprecise: see `maybe_resolve_illusion_two_in_front`'s
    /// doc comment on why this doesn't try to capture the full exclusive-or).
    #[test]
    fn test_zoroark_doubles_two_in_front_seeds_hypothesis() {
        let zoroark_back =
            UnknownPokemonState::from_opponent_species(Species::Zoroark, &HashMap::new(), 50);
        let mut state = battle_nvn(
            vec![unknown_mon_species(Species::Pikachu), unknown_mon_species(Species::Pikachu)],
            vec![unknown_mon_species(Species::Garchomp)],
        );
        state.active_per_side = 2;
        state.p2_active_mons.push(unknown_mon_species(Species::Snorlax));
        state.p2_possible_back_mons = vec![zoroark_back];
        state.p2_unresolved_zoroark_count = 1;
        state.p2_slot_conditions = vec![vec![], vec![]];

        // A SECOND Garchomp switches into slot 1 — Species Clause means the real
        // Garchomp is already in slot 0, so this one must be the disguised Zoroark.
        let result = apply(state, vec![switch_in(Species::Garchomp, p2(1))]);

        assert_eq!(
            result.p2_active_mons[1].possible_species,
            Unknown::Known(Species::Garchomp),
            "species stays pinned even for the duplicate-species slot"
        );
        assert!(
            result.p2_active_mons[1].possible_illusion_state.is_some(),
            "the newly-arrived duplicate-species slot must carry a Zoroark hypothesis"
        );
    }
}
