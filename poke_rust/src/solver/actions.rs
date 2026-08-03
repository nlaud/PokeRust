//! Returns the legal joint commands for each battle phase.
//!
//! A joint command contains one command for each active slot.
//! In doubles, both slot commands can restrict each other.
//! Therefore, matrix rows contain joint commands.
//!
//! The HTTP server and solver use this module.
//! This shared code prevents different legality rules.
//!
//! Normal phases use `validate_battle_command_combination`.
//! Replacement phases use `replacement_commands_are_valid`.
//! The replacement validator uses each available bench Pokémon before it permits an empty slot.

use std::collections::{HashMap, HashSet};

use crate::data::pokemon_move::PokemonMove;
use crate::data::species::Species;
use crate::simulator::helpers::{
    accuracy_hit_probability, calculate_damage_outcomes_for_target, effective_move_priority,
    get_pokemon_at_slot, move_has_flag,
};
use crate::simulator::{
    DamageConfig, get_possible_commands_for_active_slot, validate_battle_command_combination,
};
use crate::state::battle::{
    BattleCommand, BattleState, FieldSlot, MatchState, Player, SwitchCommand,
};
use crate::state::dex_data::{
    DamageOverride, MoveCategory, MoveData, MoveFlag, MoveTarget, PokemonData, SelfDestructType,
    SelfSwitchType, Status,
};
use crate::state::pokemon::PokemonState;
use crate::user::replacement_commands_are_valid;

/// Identifies the current input phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Phase {
    /// Leads have not been chosen yet.
    TeamPreview,
    /// The ordinary case: every healthy slot picks a move or a switch.
    Normal,
    /// A self-switch waits for one slot choice.
    /// Other slots must pass.
    SelfSwitch,
    /// The turn is over and fainted slots need replacements.
    Replacement,
    /// Terminal.
    GameOver,
}

/// A player's legal joint actions, plus how many there were before any cap.
#[derive(Debug, Clone)]
pub struct JointActions {
    /// One entry per legal joint action; each is one `BattleCommand` per active
    /// slot, in slot order.
    pub actions: Vec<Vec<BattleCommand>>,
    /// How many legal joint actions existed before `cap` was applied. Equal to
    /// `actions.len()` when nothing was dropped.
    pub total: usize,
}

impl JointActions {
    /// Returns true when a cap removed actions.
    pub fn was_capped(&self) -> bool {
        self.actions.len() < self.total
    }
}

/// Classify `state` into an input phase.
pub fn phase_of(state: &MatchState) -> Phase {
    match state {
        MatchState::TeamPreviewState(_) => Phase::TeamPreview,
        MatchState::GameOverState { .. } => Phase::GameOver,
        MatchState::BattleState(battle) => {
            if battle.self_switch_pending.is_some() {
                Phase::SelfSwitch
            } else if battle.turn_started && battle.turn_ended {
                Phase::Replacement
            } else {
                Phase::Normal
            }
        }
    }
}

fn active_mons(state: &BattleState, player: Player) -> &[PokemonState] {
    match player {
        Player::P1 => &state.p1_active_mons,
        Player::P2 => &state.p2_active_mons,
    }
}

fn back_mons(state: &BattleState, player: Player) -> &[PokemonState] {
    match player {
        Player::P1 => &state.p1_back_mons,
        Player::P2 => &state.p2_back_mons,
    }
}

/// Switch commands naming each healthy bench Pokemon.
pub fn healthy_bench_switches(state: &BattleState, player: Player) -> Vec<BattleCommand> {
    back_mons(state, player)
        .iter()
        .enumerate()
        .filter(|(_, m)| !m.fainted)
        .map(|(i, _)| BattleCommand::Switch(SwitchCommand { party_index: i }))
        .collect()
}

/// Returns independent legal commands for each active slot.
/// A slot without a choice returns one Pass.
/// This function does not apply cross-slot constraints.
pub fn per_slot_commands(
    state: &BattleState,
    player: Player,
    phase: Phase,
    move_dex: &HashMap<PokemonMove, MoveData>,
    pokemon_dex: &HashMap<Species, PokemonData>,
) -> Vec<Vec<BattleCommand>> {
    let actives = active_mons(state, player);

    (0..actives.len())
        .map(|slot_idx| match phase {
            Phase::TeamPreview | Phase::GameOver => vec![BattleCommand::Pass],

            Phase::SelfSwitch => {
                let pending = state.self_switch_pending.map(|(slot, _)| slot);
                let this_slot = FieldSlot {
                    player,
                    slot_index: slot_idx as u8,
                };
                if pending == Some(this_slot) {
                    let switches = healthy_bench_switches(state, player);
                    if switches.is_empty() {
                        vec![BattleCommand::Pass]
                    } else {
                        switches
                    }
                } else {
                    vec![BattleCommand::Pass]
                }
            }

            Phase::Replacement => {
                if actives[slot_idx].fainted {
                    let switches = healthy_bench_switches(state, player);
                    // Earlier fainted slots claim bench Pokemon first: with two
                    // fainted actives and one healthy bench Pokemon, slot 0 gets
                    // the switch and this slot is a forced Pass.
                    let earlier_fainted = actives[..slot_idx].iter().filter(|m| m.fainted).count();
                    if switches.len() <= earlier_fainted {
                        vec![BattleCommand::Pass]
                    } else {
                        switches
                    }
                } else {
                    vec![BattleCommand::Pass]
                }
            }

            Phase::Normal => get_possible_commands_for_active_slot(
                state,
                player,
                slot_idx,
                move_dex,
                pokemon_dex,
            ),
        })
        .collect()
}

/// Returns every legal joint action for one player.
/// Applies cross-slot validation after the Cartesian product.
/// `cap` can reduce the result and make the solution approximate.
/// `prune_dominated` can also reduce the result. See [`remove_dominated_actions`].
pub fn joint_actions(
    state: &BattleState,
    player: Player,
    phase: Phase,
    move_dex: &HashMap<PokemonMove, MoveData>,
    pokemon_dex: &HashMap<Species, PokemonData>,
    cap: Option<usize>,
    prune_dominated: bool,
) -> JointActions {
    if matches!(phase, Phase::TeamPreview | Phase::GameOver) {
        return JointActions {
            actions: Vec::new(),
            total: 0,
        };
    }

    let per_slot = per_slot_commands(state, player, phase, move_dex, pokemon_dex);
    let actives = active_mons(state, player);

    let mut actions: Vec<Vec<BattleCommand>> = cartesian_product(&per_slot)
        .into_iter()
        .filter(|combo| match phase {
            Phase::Replacement => replacement_commands_are_valid(state, player, actives, combo),
            _ => validate_battle_command_combination(combo),
        })
        .collect();

    // Use an all-Pass fallback when validation removes every action.
    if actions.is_empty() {
        actions.push(vec![BattleCommand::Pass; actives.len()]);
    }

    // Duplicate removal is lossless, so it runs before `total`. A capped set
    // then reports only the actions that the cap really dropped.
    actions = remove_duplicate_actions(actions, actives);

    // The dominance filter is lossy, so `total` counts the actions before it.
    // `was_capped` then reports the reduction as a truncation.
    let total = actions.len();
    if prune_dominated && matches!(phase, Phase::Normal) {
        actions = remove_dominated_actions(state, player, actions, move_dex);
    }
    if let Some(cap) = cap {
        actions = reduce_to_cap(actions, cap);
    }
    JointActions { actions, total }
}

/// The damage settings of the dominance pre-filter.
///
/// One roll keeps the pre-filter cheap. The roll multiplier scales both moves
/// of a comparison equally, so the median roll orders them as every roll does.
///
/// The estimate keeps the critical-hit branch. A critical hit ignores the
/// defensive boosts of the target, and a physical move and a special move meet
/// a different boost, so the branch can reverse the order of two moves. The
/// comparison spans the branch by reading the lowest and the highest returned
/// damage.
const PRUNE_DAMAGE_CONFIG: DamageConfig = DamageConfig {
    consider_crit: true,
    damage_rolls: 1,
    sample: false,
};

/// The damage outcomes and hit probability of one attack command.
#[derive(Debug, Clone)]
struct AttackEstimate {
    move_slot: usize,
    /// Every returned damage branch, as `(damage, is_critical, probability)`.
    /// Two commands with the same branches are the same choice.
    outcomes: Vec<(u16, bool, f64)>,
    lowest_damage: u16,
    highest_damage: u16,
    hit_probability: f64,
}

/// Whether `move_data` only deals damage to the one target that the user chose.
///
/// The pre-filter compares two moves by damage and accuracy alone, so every
/// other effect must be absent. The search cannot price an effect that the
/// comparison does not read.
///
/// The target rule keeps every spread move. A spread move also hits the ally
/// slot, so its value depends on the partner command.
fn is_plain_single_target_attack(move_data: &MoveData) -> bool {
    matches!(
        move_data.category,
        MoveCategory::Physical | MoveCategory::Special
    ) && matches!(
        move_data.target,
        MoveTarget::Normal | MoveTarget::Any | MoveTarget::AdjacentFoe
    ) && move_data.secondaries.is_empty()
        && move_data.self_secondaries.is_empty()
        && move_data.self_boost == [0i8; 7]
        && move_data.heal_fraction == [0, 0]
        && move_data.recoil_fraction == [0, 0]
        && move_data.drain_fraction == [0, 0]
        && move_data.multihit_range == [0, 0]
        && !move_data.ohko
        && !move_data.thaws_target
        && !move_data.force_switch
        && !move_data.mind_blown_recoil
        && !move_data.struggle_recoil
        && !move_data.has_crash_damage
        && !move_data.breaks_protect
        && !move_data.sleep_usable
        && matches!(move_data.self_switch, SelfSwitchType::None)
        && matches!(move_data.self_destruct, SelfDestructType::None)
        && matches!(move_data.damage_override, DamageOverride::None)
        && !move_has_flag(move_data, &MoveFlag::Charge)
        && !move_has_flag(move_data, &MoveFlag::Recharge)
        && !move_has_flag(move_data, &MoveFlag::FutureMove)
        && !has_name_based_effect(&move_data.name)
}

/// Return true when the simulator applies behavior that the move data does not describe.
/// The damage estimate does not include these effects or failure rules.
fn has_name_based_effect(move_name: &PokemonMove) -> bool {
    matches!(
        move_name,
        PokemonMove::AlluringVoice
            | PokemonMove::BeakBlast
            | PokemonMove::Belch
            | PokemonMove::BrickBreak
            | PokemonMove::BugBite
            | PokemonMove::BurnUp
            | PokemonMove::BurningJealousy
            | PokemonMove::ClearSmog
            | PokemonMove::Covet
            | PokemonMove::EerieSpell
            | PokemonMove::FakeOut
            | PokemonMove::FellStinger
            | PokemonMove::FirstImpression
            | PokemonMove::Fling
            | PokemonMove::FocusPunch
            | PokemonMove::IceBall
            | PokemonMove::IceSpinner
            | PokemonMove::KnockOff
            | PokemonMove::LastResort
            | PokemonMove::MortalSpin
            | PokemonMove::Outrage
            | PokemonMove::PetalDance
            | PokemonMove::Pluck
            | PokemonMove::PollenPuff
            | PokemonMove::Poltergeist
            | PokemonMove::PsychicFangs
            | PokemonMove::RagingBull
            | PokemonMove::RagingFury
            | PokemonMove::RapidSpin
            | PokemonMove::Rollout
            | PokemonMove::Round
            | PokemonMove::SaltCure
            | PokemonMove::SmackDown
            | PokemonMove::Snore
            | PokemonMove::SparklingAria
            | PokemonMove::SpiritShackle
            | PokemonMove::SpitUp
            | PokemonMove::SteelRoller
            | PokemonMove::SuckerPunch
            | PokemonMove::Thief
            | PokemonMove::ThroatChop
            | PokemonMove::Thrash
            | PokemonMove::UpperHand
            | PokemonMove::Uproar
    )
}

/// Groups the attack commands of one slot that the pre-filter may compare.
///
/// Two commands of one group share a target, a priority, and both resource
/// flags. A partner command applies the same multiplier to both of them, so the
/// comparison order holds under every partner command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct ComparisonGroup {
    slot_index: usize,
    target: FieldSlot,
    priority: i8,
    terastallize: bool,
}

/// Whether `better` dominates `worse`.
///
/// A dominating command must hit at least as often. It then wins in one of two
/// ways.
///
/// Two commands with the same damage branches and the same hit probability are
/// one choice. The lower move slot wins that pair, so the pair loses exactly one
/// command instead of both.
///
/// Otherwise the lowest damage of the dominating command must reach the highest
/// damage of the other command, and one of the two measures must be a strict
/// win. The strict win keeps the relation acyclic, so every group keeps at least
/// one command.
fn dominates(better: &AttackEstimate, worse: &AttackEstimate) -> bool {
    if better.hit_probability < worse.hit_probability {
        return false;
    }
    if better.outcomes == worse.outcomes && better.hit_probability == worse.hit_probability {
        return better.move_slot < worse.move_slot;
    }
    better.lowest_damage >= worse.highest_damage
        && (better.lowest_damage > worse.highest_damage
            || better.hit_probability > worse.hit_probability)
}

/// The exact slot commands that another command of the same slot dominates.
fn dominated_slot_commands(
    state: &BattleState,
    player: Player,
    actions: &[Vec<BattleCommand>],
    move_dex: &HashMap<PokemonMove, MoveData>,
) -> HashSet<(usize, ExactSlotKey)> {
    let mut groups: HashMap<ComparisonGroup, Vec<AttackEstimate>> = HashMap::new();
    let mut seen: HashSet<(usize, ExactSlotKey)> = HashSet::new();

    for combo in actions {
        for (slot_index, command) in combo.iter().enumerate() {
            if !seen.insert((slot_index, exact_slot_key(command))) {
                continue;
            }
            let Some((group, estimate)) =
                attack_estimate(state, player, slot_index, command, move_dex)
            else {
                continue;
            };
            groups.entry(group).or_default().push(estimate);
        }
    }

    let mut dominated: HashSet<(usize, ExactSlotKey)> = HashSet::new();
    for (group, estimates) in &groups {
        for worse in estimates {
            if !estimates.iter().any(|better| dominates(better, worse)) {
                continue;
            }
            dominated.insert((
                group.slot_index,
                ExactSlotKey::Attack {
                    move_slot: worse.move_slot,
                    target: Some(group.target),
                    terastallize: group.terastallize,
                    mega_evolve: false,
                },
            ));
        }
    }
    dominated
}

/// The comparison group and estimate of one attack command, when the pre-filter
/// may compare that command.
///
/// A Mega Evolution returns `None`. It changes the species, the stats, and the
/// ability of the user, and this estimate reads the current form.
fn attack_estimate(
    state: &BattleState,
    player: Player,
    slot_index: usize,
    command: &BattleCommand,
    move_dex: &HashMap<PokemonMove, MoveData>,
) -> Option<(ComparisonGroup, AttackEstimate)> {
    let BattleCommand::Attack(attack) = command else {
        return None;
    };
    if attack.mega_evolve {
        return None;
    }
    let target_slot = attack.target?;
    let attacker = active_mons(state, player).get(slot_index)?;

    // A sleeping or frozen user runs a different move set, and the damage
    // estimate does not read the status. Leave both cases to the search.
    if matches!(attacker.status, Some(Status::Sleep(_)) | Some(Status::Frozen(_))) {
        return None;
    }

    let move_name = attacker.moves.get(attack.move_slot)?.as_ref()?;
    let move_data = move_dex.get(move_name)?;
    if !is_plain_single_target_attack(move_data) {
        return None;
    }

    let target = get_pokemon_at_slot(state, target_slot)?;
    if target.fainted {
        return None;
    }

    // Terastallization only sets `is_tera`, so a copy models it exactly. The
    // copy grants the Tera type its STAB and leaves every other field alone.
    let user_slot = FieldSlot {
        player,
        slot_index: slot_index as u8,
    };
    let mut attacker_copy;
    let attacker = if attack.terastallize {
        attacker_copy = attacker.clone();
        attacker_copy.is_tera = true;
        &attacker_copy
    } else {
        attacker
    };

    let outcomes = calculate_damage_outcomes_for_target(
        state,
        attacker,
        target,
        user_slot,
        target_slot,
        move_data,
        PRUNE_DAMAGE_CONFIG,
        1.0,
        1.0,
    );
    let lowest_damage = outcomes.iter().map(|(damage, _, _)| *damage).min()?;
    let highest_damage = outcomes.iter().map(|(damage, _, _)| *damage).max()?;
    let hit_probability = accuracy_hit_probability(
        state,
        attacker,
        target,
        user_slot,
        target_slot,
        move_data,
    );

    Some((
        ComparisonGroup {
            slot_index,
            target: target_slot,
            priority: effective_move_priority(state, attacker, move_data),
            terastallize: attack.terastallize,
        },
        AttackEstimate {
            move_slot: attack.move_slot,
            outcomes,
            lowest_damage,
            highest_damage,
            hit_probability,
        },
    ))
}

/// Removes each joint action that holds a dominated slot command.
///
/// The filter is approximate, so a caller opts in with
/// `SolveConfig::prune_dominated_actions`.
///
/// A dominating command shares the target and both resource flags with the
/// command that it replaces, so the swap keeps the joint action legal and the
/// set never becomes empty. The empty check below covers that claim at runtime.
fn remove_dominated_actions(
    state: &BattleState,
    player: Player,
    actions: Vec<Vec<BattleCommand>>,
    move_dex: &HashMap<PokemonMove, MoveData>,
) -> Vec<Vec<BattleCommand>> {
    let dominated = dominated_slot_commands(state, player, &actions, move_dex);
    if dominated.is_empty() {
        return actions;
    }

    let kept: Vec<Vec<BattleCommand>> = actions
        .iter()
        .filter(|combo| {
            !combo
                .iter()
                .enumerate()
                .any(|(slot_index, command)| {
                    dominated.contains(&(slot_index, exact_slot_key(command)))
                })
        })
        .cloned()
        .collect();

    if kept.is_empty() { actions } else { kept }
}

/// The battle resources that one joint action spends.
///
/// A joint action spends at most one Tera and at most one Mega.
/// `validate_battle_command_combination` rejects the other combinations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum ResourceChoice {
    Plain,
    Tera,
    Mega,
    TeraMega,
}

/// The resource groups, in the order that the cap gives them a share.
const RESOURCE_CHOICES: [ResourceChoice; 4] = [
    ResourceChoice::Plain,
    ResourceChoice::Tera,
    ResourceChoice::Mega,
    ResourceChoice::TeraMega,
];

fn resource_choice(combo: &[BattleCommand]) -> ResourceChoice {
    let mut tera = false;
    let mut mega = false;
    for command in combo {
        if let BattleCommand::Attack(attack) = command {
            tera |= attack.terastallize;
            mega |= attack.mega_evolve;
        }
    }
    match (tera, mega) {
        (false, false) => ResourceChoice::Plain,
        (true, false) => ResourceChoice::Tera,
        (false, true) => ResourceChoice::Mega,
        (true, true) => ResourceChoice::TeraMega,
    }
}

/// One slot command without its target or resource flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum SlotKey {
    Pass,
    Struggle,
    Switch(usize),
    Attack(usize),
}

/// One complete slot command for action-cap coverage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum ExactSlotKey {
    Pass,
    Struggle(Option<FieldSlot>),
    Switch(usize),
    Attack {
        move_slot: usize,
        target: Option<FieldSlot>,
        terastallize: bool,
        mega_evolve: bool,
    },
}

fn exact_slot_key(command: &BattleCommand) -> ExactSlotKey {
    match command {
        BattleCommand::Pass => ExactSlotKey::Pass,
        BattleCommand::Struggle { target } => ExactSlotKey::Struggle(*target),
        BattleCommand::Switch(switch) => ExactSlotKey::Switch(switch.party_index),
        BattleCommand::Attack(attack) => ExactSlotKey::Attack {
            move_slot: attack.move_slot,
            target: attack.target,
            terastallize: attack.terastallize,
            mega_evolve: attack.mega_evolve,
        },
    }
}

/// Identifies the slots that use Tera and Mega in one joint action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct ResourceAssignment {
    tera_slot: Option<usize>,
    mega_slot: Option<usize>,
}

fn resource_assignment(combo: &[BattleCommand]) -> ResourceAssignment {
    let mut assignment = ResourceAssignment {
        tera_slot: None,
        mega_slot: None,
    };
    for (slot_idx, command) in combo.iter().enumerate() {
        if let BattleCommand::Attack(attack) = command {
            if attack.terastallize {
                assignment.tera_slot = Some(slot_idx);
            }
            if attack.mega_evolve {
                assignment.mega_slot = Some(slot_idx);
            }
        }
    }
    assignment
}

fn slot_key(command: &BattleCommand) -> SlotKey {
    match command {
        BattleCommand::Pass => SlotKey::Pass,
        BattleCommand::Struggle { .. } => SlotKey::Struggle,
        BattleCommand::Switch(switch) => SlotKey::Switch(switch.party_index),
        BattleCommand::Attack(attack) => SlotKey::Attack(attack.move_slot),
    }
}

/// The lowest move slot that holds the same move with the same PP.
///
/// A Pokemon can know one move in two slots. Both commands queue the same move
/// name. Move execution also finds the PP slot by that name.
///
/// The equal-PP check makes this reduction conservative. Name-based effects,
/// such as Disable and Choice Lock, also treat the commands equally.
fn canonical_move_slot(mon: &PokemonState, move_slot: usize) -> usize {
    if move_slot >= mon.moves.len() {
        return move_slot;
    }
    let Some(name) = mon.moves[move_slot].as_ref() else {
        return move_slot;
    };
    let pp = mon.move_pp[move_slot];
    (0..move_slot)
        .find(|&earlier| mon.moves[earlier].as_ref() == Some(name) && mon.move_pp[earlier] == pp)
        .unwrap_or(move_slot)
}

/// One slot command with its move slot replaced by the canonical move slot.
/// The target and both resource flags stay in the key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct DuplicateKey {
    command: SlotKey,
    target: Option<FieldSlot>,
    terastallize: bool,
    mega_evolve: bool,
}

/// One joint action with each repeated move slot replaced by its canonical slot.
/// Two joint actions with the same key have the same value.
///
/// The key holds the resource flags of each slot. Two joint actions that
/// Terastallize a different slot therefore keep a different key.
fn duplicate_key(combo: &[BattleCommand], actives: &[PokemonState]) -> Vec<DuplicateKey> {
    combo
        .iter()
        .enumerate()
        .map(|(slot_idx, command)| match command {
            BattleCommand::Attack(attack) => DuplicateKey {
                command: match actives.get(slot_idx) {
                    Some(mon) => SlotKey::Attack(canonical_move_slot(mon, attack.move_slot)),
                    None => SlotKey::Attack(attack.move_slot),
                },
                target: attack.target,
                terastallize: attack.terastallize,
                mega_evolve: attack.mega_evolve,
            },
            BattleCommand::Struggle { target } => DuplicateKey {
                command: SlotKey::Struggle,
                target: *target,
                terastallize: false,
                mega_evolve: false,
            },
            other => DuplicateKey {
                command: slot_key(other),
                target: None,
                terastallize: false,
                mega_evolve: false,
            },
        })
        .collect()
}

/// Removes the joint actions that repeat the value of an earlier joint action.
/// The resource flags stay in the key, so this step never drops a Tera or a Mega.
fn remove_duplicate_actions(
    actions: Vec<Vec<BattleCommand>>,
    actives: &[PokemonState],
) -> Vec<Vec<BattleCommand>> {
    let mut seen: HashSet<Vec<DuplicateKey>> = HashSet::new();
    actions
        .into_iter()
        .filter(|combo| seen.insert(duplicate_key(combo, actives)))
        .collect()
}

/// Reduces a joint-action set to `cap` entries.
///
/// The reduction runs in three steps.
///
/// 1. Group the actions by resource choice.
/// 2. Give each non-empty group a share of the cap.
/// 3. Take actions that cover exact slot commands and resource assignments.
///
/// Step 2 keeps Tera and Mega actions when the cap has sufficient space.
/// Step 3 prevents a row-major bias toward one slot, target, or resource user.
///
/// The result is a pure function of the input, so each search pass over one
/// position builds the same list. `RootSeed` needs that property.
fn reduce_to_cap(actions: Vec<Vec<BattleCommand>>, cap: usize) -> Vec<Vec<BattleCommand>> {
    let cap = cap.max(1);
    if actions.len() <= cap {
        return actions;
    }

    let choices: Vec<ResourceChoice> = actions.iter().map(|combo| resource_choice(combo)).collect();
    let groups: Vec<Vec<usize>> = RESOURCE_CHOICES
        .iter()
        .map(|choice| {
            (0..actions.len())
                .filter(|&index| choices[index] == *choice)
                .collect::<Vec<usize>>()
        })
        .filter(|group| !group.is_empty())
        .collect();

    let sizes: Vec<usize> = groups.iter().map(|group| group.len()).collect();
    let shares = allocate_shares(cap, &sizes);

    let mut kept: Vec<usize> = Vec::with_capacity(cap);
    for (group, share) in groups.iter().zip(&shares) {
        kept.extend(select_by_coverage(group, &actions, *share));
    }

    // The original order keeps the action indices readable in a debug log.
    kept.sort_unstable();
    kept.into_iter()
        .map(|index| actions[index].clone())
        .collect()
}

/// Splits `cap` over the groups by the largest-remainder method.
/// Every non-empty group keeps at least one action while the cap permits it.
fn allocate_shares(cap: usize, sizes: &[usize]) -> Vec<usize> {
    let total: usize = sizes.iter().sum();
    if total == 0 {
        return vec![0; sizes.len()];
    }

    let mut shares: Vec<usize> = Vec::with_capacity(sizes.len());
    let mut remainders: Vec<(usize, usize)> = Vec::with_capacity(sizes.len());
    for (index, &size) in sizes.iter().enumerate() {
        let exact = cap * size;
        shares.push(exact / total);
        remainders.push((exact % total, index));
    }

    // The largest remainder takes the first leftover seat. A tie goes to the
    // earlier group, so the split stays deterministic.
    remainders.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    let mut leftover = cap.min(total) - shares.iter().sum::<usize>();
    while leftover > 0 {
        let mut moved = false;
        for &(_, index) in &remainders {
            if leftover == 0 {
                break;
            }
            if shares[index] < sizes[index] {
                shares[index] += 1;
                leftover -= 1;
                moved = true;
            }
        }
        if !moved {
            break;
        }
    }

    // Raise an empty group to one seat. The donor is the earliest largest group
    // that keeps a seat after the transfer.
    for index in 0..shares.len() {
        if shares[index] > 0 {
            continue;
        }
        let mut donor: Option<usize> = None;
        for other in 0..shares.len() {
            if shares[other] > 1 && donor.is_none_or(|best| shares[other] > shares[best]) {
                donor = Some(other);
            }
        }
        match donor {
            Some(other) => {
                shares[other] -= 1;
                shares[index] = 1;
            }
            None => break,
        }
    }

    shares
}

/// Takes `want` actions that cover the available choices of each slot.
///
/// Resource assignments have first priority. Base slot commands have second
/// priority. Exact commands have third priority and include targets.
///
/// A tie uses the earlier action. This rule makes the result deterministic.
fn select_by_coverage(group: &[usize], actions: &[Vec<BattleCommand>], want: usize) -> Vec<usize> {
    if want == 0 {
        return Vec::new();
    }

    let slots = group
        .first()
        .and_then(|&index| actions.get(index))
        .map_or(0, Vec::len);
    let mut covered_base_commands: Vec<HashSet<SlotKey>> = vec![HashSet::new(); slots];
    let mut covered_commands: Vec<HashSet<ExactSlotKey>> = vec![HashSet::new(); slots];
    let mut covered_resources: HashSet<ResourceAssignment> = HashSet::new();
    let mut used: HashSet<usize> = HashSet::new();
    let mut taken: Vec<usize> = Vec::with_capacity(want.min(group.len()));
    for _ in 0..want.min(group.len()) {
        let mut best: Option<((usize, usize, usize), usize)> = None;
        for &index in group {
            if used.contains(&index) {
                continue;
            }
            let assignment = resource_assignment(&actions[index]);
            let resource_gain = usize::from(!covered_resources.contains(&assignment));
            let base_gain = actions[index]
                .iter()
                .enumerate()
                .filter(|(slot_idx, command)| {
                    !covered_base_commands[*slot_idx].contains(&slot_key(command))
                })
                .count();
            let exact_gain = actions[index]
                .iter()
                .enumerate()
                .filter(|(slot_idx, command)| {
                    !covered_commands[*slot_idx].contains(&exact_slot_key(command))
                })
                .count();
            let score = (resource_gain, base_gain, exact_gain);
            if best.is_none_or(|(top, _)| score > top) {
                best = Some((score, index));
            }
        }
        let Some((score, mut index)) = best else {
            break;
        };
        if score == (0, 0, 0) {
            // Start a new sweep after the selected actions cover all choices.
            for slot in &mut covered_base_commands {
                slot.clear();
            }
            for slot in &mut covered_commands {
                slot.clear();
            }
            covered_resources.clear();
            index = group
                .iter()
                .copied()
                .find(|candidate| !used.contains(candidate))
                .unwrap_or(index);
        }
        covered_resources.insert(resource_assignment(&actions[index]));
        for (slot_idx, command) in actions[index].iter().enumerate() {
            covered_base_commands[slot_idx].insert(slot_key(command));
            covered_commands[slot_idx].insert(exact_slot_key(command));
        }
        used.insert(index);
        taken.push(index);
    }
    taken
}

/// Orders every joint action by coverage.
///
/// The result is a permutation of `0..actions.len()`.
/// Each prefix of that permutation covers as many distinct resource
/// assignments, slot commands, and targets as its length permits.
///
/// [`reduce_to_cap`] applies the same rule inside one resource group.
/// This function runs one sweep over the whole list, so a caller that grows an
/// action set can extend a prefix instead of rebuilding the set.
///
/// The cost is quadratic in the action count.
/// Call it once for each action set.
pub fn coverage_order(actions: &[Vec<BattleCommand>]) -> Vec<usize> {
    let group: Vec<usize> = (0..actions.len()).collect();
    select_by_coverage(&group, actions, actions.len())
}

/// Every combination taking one element from each list, in row-major order.
fn cartesian_product(per_slot: &[Vec<BattleCommand>]) -> Vec<Vec<BattleCommand>> {
    let mut combos: Vec<Vec<BattleCommand>> = vec![Vec::with_capacity(per_slot.len())];
    for options in per_slot {
        combos = combos
            .into_iter()
            .flat_map(|prefix| {
                options.iter().map(move |option| {
                    let mut extended = prefix.clone();
                    extended.push(option.clone());
                    extended
                })
            })
            .collect();
    }
    combos
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::species::Species;
    use crate::state::battle::AttackCommand;
    use crate::state::pokemon::build_pokemon_state;
    use crate::tests::simuilator_test_helpers::{battle_state_from_lists, move_dex, pokemon_dex};

    fn mon(species: Species, moves: [Option<PokemonMove>; 4]) -> PokemonState {
        build_pokemon_state(
            species,
            pokemon_dex(),
            move_dex(),
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
        )
    }

    fn four_moves() -> [Option<PokemonMove>; 4] {
        [
            Some(PokemonMove::Tackle),
            Some(PokemonMove::Growl),
            Some(PokemonMove::Protect),
            Some(PokemonMove::Splash),
        ]
    }

    #[test]
    fn singles_joint_actions_are_just_the_slot_options() {
        let state = battle_state_from_lists(
            vec![mon(Species::Pikachu, four_moves())],
            vec![mon(Species::Snorlax, four_moves())],
            vec![mon(Species::Pikachu, four_moves())],
            vec![],
        );
        let joint = joint_actions(
            &state,
            Player::P1,
            Phase::Normal,
            move_dex(),
            pokemon_dex(),
            None,
            false,
        );
        assert!(joint.actions.iter().all(|combo| combo.len() == 1));
        // Four moves plus one bench switch, at minimum.
        assert!(joint.actions.len() >= 5, "got {}", joint.actions.len());
        assert_eq!(joint.total, joint.actions.len());
        assert!(!joint.was_capped());
    }

    #[test]
    fn doubles_joint_actions_pair_the_slots_and_reject_duplicate_switches() {
        let state = battle_state_from_lists(
            vec![
                mon(Species::Pikachu, four_moves()),
                mon(Species::Snorlax, four_moves()),
            ],
            vec![mon(Species::Gengar, four_moves())],
            vec![
                mon(Species::Pikachu, four_moves()),
                mon(Species::Snorlax, four_moves()),
            ],
            vec![mon(Species::Gengar, four_moves())],
        );
        let joint = joint_actions(
            &state,
            Player::P1,
            Phase::Normal,
            move_dex(),
            pokemon_dex(),
            None,
            false,
        );
        assert!(joint.actions.iter().all(|combo| combo.len() == 2));
        // With exactly one bench Pokemon, no joint action may switch both slots.
        for combo in &joint.actions {
            let switches = combo
                .iter()
                .filter(|c| matches!(c, BattleCommand::Switch(_)))
                .count();
            assert!(switches <= 1, "both slots switched to the same Pokemon");
        }
    }

    #[test]
    fn capping_reduces_the_set_and_records_the_original_size() {
        let state = battle_state_from_lists(
            vec![
                mon(Species::Pikachu, four_moves()),
                mon(Species::Snorlax, four_moves()),
            ],
            vec![mon(Species::Gengar, four_moves())],
            vec![
                mon(Species::Pikachu, four_moves()),
                mon(Species::Snorlax, four_moves()),
            ],
            vec![mon(Species::Gengar, four_moves())],
        );
        let uncapped = joint_actions(
            &state,
            Player::P1,
            Phase::Normal,
            move_dex(),
            pokemon_dex(),
            None,
            false,
        );
        let capped = joint_actions(
            &state,
            Player::P1,
            Phase::Normal,
            move_dex(),
            pokemon_dex(),
            Some(6),
            false,
        );
        assert!(capped.actions.len() <= 6);
        assert_eq!(capped.total, uncapped.actions.len());
        assert!(capped.was_capped());
        // Every retained action must still be legal.
        for combo in &capped.actions {
            assert!(validate_battle_command_combination(combo));
        }
    }

    /// A cap never removes a player's ability to act.
    #[test]
    fn capping_never_empties_the_action_set() {
        let state = battle_state_from_lists(
            vec![mon(Species::Pikachu, four_moves())],
            vec![],
            vec![mon(Species::Pikachu, four_moves())],
            vec![],
        );
        let capped = joint_actions(
            &state,
            Player::P1,
            Phase::Normal,
            move_dex(),
            pokemon_dex(),
            Some(0),
            false,
        );
        assert!(!capped.actions.is_empty());
    }

    #[test]
    fn a_fainted_slot_with_an_empty_bench_can_only_pass() {
        let mut state = battle_state_from_lists(
            vec![mon(Species::Pikachu, four_moves())],
            vec![],
            vec![mon(Species::Pikachu, four_moves())],
            vec![],
        );
        state.p1_active_mons[0].hp = 0;
        state.p1_active_mons[0].fainted = true;
        state.turn_started = true;
        state.turn_ended = true;

        let phase = phase_of(&MatchState::BattleState(state.clone()));
        assert_eq!(phase, Phase::Replacement);

        let joint = joint_actions(&state, Player::P1, phase, move_dex(), pokemon_dex(), None, false);
        assert_eq!(joint.actions, vec![vec![BattleCommand::Pass]]);
    }

    #[test]
    fn a_replacement_offers_every_healthy_bench_pokemon() {
        let mut state = battle_state_from_lists(
            vec![mon(Species::Pikachu, four_moves())],
            vec![
                mon(Species::Snorlax, four_moves()),
                mon(Species::Gengar, four_moves()),
            ],
            vec![mon(Species::Pikachu, four_moves())],
            vec![],
        );
        state.p1_active_mons[0].hp = 0;
        state.p1_active_mons[0].fainted = true;
        state.turn_started = true;
        state.turn_ended = true;

        let joint = joint_actions(
            &state,
            Player::P1,
            Phase::Replacement,
            move_dex(),
            pokemon_dex(),
            None,
            false,
        );
        assert_eq!(joint.actions.len(), 2);
        assert!(
            joint
                .actions
                .iter()
                .all(|combo| matches!(combo.as_slice(), [BattleCommand::Switch(_)]))
        );
    }

    /// The healthy partner of a fainted slot must not get to act again during a
    /// replacement — it already moved this turn.
    #[test]
    fn a_healthy_slot_passes_during_a_replacement() {
        let mut state = battle_state_from_lists(
            vec![
                mon(Species::Pikachu, four_moves()),
                mon(Species::Snorlax, four_moves()),
            ],
            vec![mon(Species::Gengar, four_moves())],
            vec![
                mon(Species::Pikachu, four_moves()),
                mon(Species::Snorlax, four_moves()),
            ],
            vec![mon(Species::Gengar, four_moves())],
        );
        state.p1_active_mons[0].hp = 0;
        state.p1_active_mons[0].fainted = true;
        state.turn_started = true;
        state.turn_ended = true;

        let joint = joint_actions(
            &state,
            Player::P1,
            Phase::Replacement,
            move_dex(),
            pokemon_dex(),
            None,
            false,
        );
        assert!(!joint.actions.is_empty());
        for combo in &joint.actions {
            assert!(matches!(combo[1], BattleCommand::Pass));
        }
    }

    /// The old reduction removed every Tera and Mega action before it applied
    /// the cap, so a capped search never studied the Tera resource.
    #[test]
    fn a_cap_keeps_a_tera_action() {
        let state = battle_state_from_lists(
            vec![mon(Species::Pikachu, four_moves())],
            vec![mon(Species::Snorlax, four_moves())],
            vec![mon(Species::Gengar, four_moves())],
            vec![],
        );
        let uncapped = joint_actions(
            &state,
            Player::P1,
            Phase::Normal,
            move_dex(),
            pokemon_dex(),
            None,
            false,
        );
        assert!(
            uncapped.actions.iter().any(|combo| uses_tera(combo)),
            "the position offers no Tera action"
        );

        let capped = joint_actions(
            &state,
            Player::P1,
            Phase::Normal,
            move_dex(),
            pokemon_dex(),
            Some(6),
            false,
        );
        assert!(capped.actions.len() <= 6);
        assert!(
            capped.actions.iter().any(|combo| uses_tera(combo)),
            "the cap removed every Tera action: {:?}",
            capped.actions
        );
        assert!(
            capped.actions.iter().any(|combo| !uses_tera(combo)),
            "the cap removed every plain action: {:?}",
            capped.actions
        );
    }

    fn uses_tera(combo: &[BattleCommand]) -> bool {
        combo
            .iter()
            .any(|command| matches!(command, BattleCommand::Attack(a) if a.terastallize))
    }

    /// The old reduction kept one action per fixed stride over a row-major list.
    /// Slot 0 changes slowest in that list, so a stride kept one slot 0 command.
    #[test]
    fn a_cap_covers_every_slot_command() {
        let state = battle_state_from_lists(
            vec![
                mon(Species::Pikachu, four_moves()),
                mon(Species::Snorlax, four_moves()),
            ],
            vec![mon(Species::Gengar, four_moves())],
            vec![
                mon(Species::Gengar, four_moves()),
                mon(Species::Snorlax, four_moves()),
            ],
            vec![mon(Species::Pikachu, four_moves())],
        );
        let uncapped = joint_actions(
            &state,
            Player::P1,
            Phase::Normal,
            move_dex(),
            pokemon_dex(),
            None,
            false,
        );

        // The selection runs inside one resource group. Its first sweep must
        // cover the commands of both slots.
        let plain: Vec<usize> = (0..uncapped.actions.len())
            .filter(|&index| resource_choice(&uncapped.actions[index]) == ResourceChoice::Plain)
            .collect();
        let tuples = distinct_tuples(&plain, &uncapped.actions);
        let ordered = select_by_coverage(&plain, &uncapped.actions, plain.len());
        assert!(ordered.len() > tuples, "the group holds no second target");
        let prefix: Vec<Vec<BattleCommand>> = ordered[..tuples]
            .iter()
            .map(|&index| uncapped.actions[index].clone())
            .collect();
        let whole: Vec<Vec<BattleCommand>> = plain
            .iter()
            .map(|&index| uncapped.actions[index].clone())
            .collect();
        for slot_idx in 0..2 {
            assert_eq!(
                keys_of_slot(&prefix, slot_idx),
                keys_of_slot(&whole, slot_idx),
                "the order dropped a slot {} command",
                slot_idx
            );
        }

        // The row-major order of the same length keeps one slot 0 command,
        // because slot 0 changes slowest in that order.
        assert!(
            keys_of_slot(&whole[..tuples], 0).len() < keys_of_slot(&whole, 0).len(),
            "the row-major order already covered slot 0"
        );

        // A tight cap must still spread over both slots.
        let tight = joint_actions(
            &state,
            Player::P1,
            Phase::Normal,
            move_dex(),
            pokemon_dex(),
            Some(9),
            false,
        );
        for slot_idx in 0..2 {
            assert!(
                keys_of_slot(&tight.actions, slot_idx).len() >= 3,
                "slot {} kept {:?}",
                slot_idx,
                keys_of_slot(&tight.actions, slot_idx)
            );
        }
    }

    fn attack(move_slot: usize, target: FieldSlot, terastallize: bool) -> BattleCommand {
        BattleCommand::Attack(AttackCommand {
            move_slot,
            target: Some(target),
            terastallize,
            mega_evolve: false,
        })
    }

    /// Row-major generation puts the slot 1 Tera variant first for every move
    /// tuple. The cap must also keep a slot 0 Tera variant.
    #[test]
    fn capped_resource_group_covers_each_resource_user() {
        let target = FieldSlot {
            player: Player::P2,
            slot_index: 0,
        };
        let actions = vec![
            vec![attack(0, target, false), attack(0, target, true)],
            vec![attack(0, target, true), attack(0, target, false)],
            vec![attack(1, target, false), attack(1, target, true)],
            vec![attack(1, target, true), attack(1, target, false)],
        ];
        let group: Vec<usize> = (0..actions.len()).collect();
        let selected = select_by_coverage(&group, &actions, 2);
        let tera_slots: HashSet<usize> = selected
            .iter()
            .flat_map(|&index| {
                actions[index]
                    .iter()
                    .enumerate()
                    .filter_map(|(slot_idx, command)| {
                        matches!(command, BattleCommand::Attack(attack) if attack.terastallize)
                            .then_some(slot_idx)
                    })
            })
            .collect();

        assert_eq!(tera_slots, HashSet::from([0, 1]));
    }

    /// Row-major generation puts the first target first for every command
    /// tuple. The cap must cover the targets of both slots.
    #[test]
    fn capped_resource_group_covers_each_target() {
        let target_0 = FieldSlot {
            player: Player::P2,
            slot_index: 0,
        };
        let target_1 = FieldSlot {
            player: Player::P2,
            slot_index: 1,
        };
        let actions = vec![
            vec![attack(0, target_0, false), attack(0, target_0, false)],
            vec![attack(0, target_0, false), attack(0, target_1, false)],
            vec![attack(0, target_1, false), attack(0, target_0, false)],
            vec![attack(0, target_1, false), attack(0, target_1, false)],
        ];
        let group: Vec<usize> = (0..actions.len()).collect();
        let selected = select_by_coverage(&group, &actions, 2);

        let targets_of = |slot_idx: usize| -> HashSet<Option<FieldSlot>> {
            selected
                .iter()
                .map(|&index| match &actions[index][slot_idx] {
                    BattleCommand::Attack(attack) => attack.target,
                    _ => None,
                })
                .collect()
        };
        let both = HashSet::from([Some(target_0), Some(target_1)]);
        assert_eq!(targets_of(0), both);
        assert_eq!(targets_of(1), both);
    }

    fn distinct_tuples(group: &[usize], actions: &[Vec<BattleCommand>]) -> usize {
        group
            .iter()
            .map(|&index| {
                actions[index]
                    .iter()
                    .map(slot_key)
                    .collect::<Vec<SlotKey>>()
            })
            .collect::<HashSet<Vec<SlotKey>>>()
            .len()
    }

    fn keys_of_slot(actions: &[Vec<BattleCommand>], slot_idx: usize) -> Vec<SlotKey> {
        let mut keys: Vec<SlotKey> = actions
            .iter()
            .map(|combo| slot_key(&combo[slot_idx]))
            .collect::<HashSet<SlotKey>>()
            .into_iter()
            .collect();
        keys.sort_by_key(|key| format!("{:?}", key));
        keys
    }

    /// `RootSeed` carries action indices from one deepening pass to the next, so
    /// one position must always build the same list.
    #[test]
    fn a_cap_is_deterministic() {
        let state = battle_state_from_lists(
            vec![
                mon(Species::Pikachu, four_moves()),
                mon(Species::Snorlax, four_moves()),
            ],
            vec![mon(Species::Gengar, four_moves())],
            vec![
                mon(Species::Gengar, four_moves()),
                mon(Species::Snorlax, four_moves()),
            ],
            vec![mon(Species::Pikachu, four_moves())],
        );
        let first = joint_actions(
            &state,
            Player::P1,
            Phase::Normal,
            move_dex(),
            pokemon_dex(),
            Some(7),
            false,
        );
        for _ in 0..5 {
            let again = joint_actions(
                &state,
                Player::P1,
                Phase::Normal,
                move_dex(),
                pokemon_dex(),
                Some(7),
                false,
            );
            assert_eq!(
                format!("{:?}", first.actions),
                format!("{:?}", again.actions)
            );
        }
    }

    /// Two slots that hold the same move with the same PP queue the same move.
    /// The name-based PP update gives both commands one value.
    #[test]
    fn duplicate_move_slots_collapse() {
        let repeated = [
            Some(PokemonMove::Tackle),
            Some(PokemonMove::Tackle),
            Some(PokemonMove::Protect),
            Some(PokemonMove::Splash),
        ];
        let state = battle_state_from_lists(
            vec![mon(Species::Pikachu, repeated)],
            vec![],
            vec![mon(Species::Gengar, four_moves())],
            vec![],
        );
        let joint = joint_actions(
            &state,
            Player::P1,
            Phase::Normal,
            move_dex(),
            pokemon_dex(),
            None,
            false,
        );
        assert!(
            joint
                .actions
                .iter()
                .any(|combo| matches!(&combo[0], BattleCommand::Attack(a) if a.move_slot == 0)),
            "the first Tackle slot disappeared"
        );
        assert!(
            !joint
                .actions
                .iter()
                .any(|combo| matches!(&combo[0], BattleCommand::Attack(a) if a.move_slot == 1)),
            "the repeated Tackle slot survived: {:?}",
            joint.actions
        );
    }

    /// Two slots that hold the same move are not the same choice when one slot
    /// Terastallizes. The duplicate key holds the resource flags of each slot.
    #[test]
    fn a_tera_on_each_slot_stays_distinct() {
        let state = battle_state_from_lists(
            vec![
                mon(Species::Pikachu, four_moves()),
                mon(Species::Pikachu, four_moves()),
            ],
            vec![],
            vec![
                mon(Species::Gengar, four_moves()),
                mon(Species::Snorlax, four_moves()),
            ],
            vec![],
        );
        let joint = joint_actions(
            &state,
            Player::P1,
            Phase::Normal,
            move_dex(),
            pokemon_dex(),
            None,
            false,
        );
        for tera_slot in 0..2 {
            assert!(
                joint.actions.iter().any(|combo| {
                    combo.iter().enumerate().all(|(slot_idx, command)| {
                        matches!(command, BattleCommand::Attack(a)
                            if a.terastallize == (slot_idx == tera_slot))
                    })
                }),
                "no action Terastallizes slot {} alone",
                tera_slot
            );
        }
    }

    /// A different move in each slot is a real choice, so nothing collapses.
    #[test]
    fn distinct_move_slots_all_survive() {
        let state = battle_state_from_lists(
            vec![mon(Species::Pikachu, four_moves())],
            vec![],
            vec![mon(Species::Gengar, four_moves())],
            vec![],
        );
        let joint = joint_actions(
            &state,
            Player::P1,
            Phase::Normal,
            move_dex(),
            pokemon_dex(),
            None,
            false,
        );
        for move_slot in 0..4 {
            assert!(
                joint.actions.iter().any(
                    |combo| matches!(&combo[0], BattleCommand::Attack(a) if a.move_slot == move_slot)
                ),
                "move slot {} disappeared",
                move_slot
            );
        }
    }

    /// A repeated move with unequal PP is a real choice: the two slots offer a
    /// different number of later uses.
    #[test]
    fn a_repeated_move_with_unequal_pp_survives() {
        let repeated = [
            Some(PokemonMove::Tackle),
            Some(PokemonMove::Tackle),
            Some(PokemonMove::Protect),
            Some(PokemonMove::Splash),
        ];
        let mut state = battle_state_from_lists(
            vec![mon(Species::Pikachu, repeated)],
            vec![],
            vec![mon(Species::Gengar, four_moves())],
            vec![],
        );
        state.p1_active_mons[0].move_pp[1] -= 1;

        let joint = joint_actions(
            &state,
            Player::P1,
            Phase::Normal,
            move_dex(),
            pokemon_dex(),
            None,
            false,
        );
        for move_slot in 0..2 {
            assert!(
                joint.actions.iter().any(
                    |combo| matches!(&combo[0], BattleCommand::Attack(a) if a.move_slot == move_slot)
                ),
                "move slot {} disappeared",
                move_slot
            );
        }
    }

    /// The cap never returns more actions than the caller asked for, and it
    /// never returns fewer while the position offers more.
    #[test]
    fn a_cap_fills_every_seat() {
        for cap in 1..=12 {
            let state = battle_state_from_lists(
                vec![
                    mon(Species::Pikachu, four_moves()),
                    mon(Species::Snorlax, four_moves()),
                ],
                vec![mon(Species::Gengar, four_moves())],
                vec![
                    mon(Species::Gengar, four_moves()),
                    mon(Species::Snorlax, four_moves()),
                ],
                vec![mon(Species::Pikachu, four_moves())],
            );
            let capped = joint_actions(
                &state,
                Player::P1,
                Phase::Normal,
                move_dex(),
                pokemon_dex(),
                Some(cap),
                false,
            );
            assert_eq!(capped.actions.len(), cap.min(capped.total), "cap {}", cap);
            for combo in &capped.actions {
                assert!(validate_battle_command_combination(combo));
            }
        }
    }

    #[test]
    fn phase_classification() {
        let state = battle_state_from_lists(
            vec![mon(Species::Pikachu, four_moves())],
            vec![],
            vec![mon(Species::Pikachu, four_moves())],
            vec![],
        );
        assert_eq!(
            phase_of(&MatchState::BattleState(state.clone())),
            Phase::Normal
        );
        assert_eq!(
            phase_of(&MatchState::GameOverState {
                winner: Player::P1,
                pending_events: Vec::new(),
                final_state: Box::new(state),
            }),
            Phase::GameOver
        );
    }

    /// A singles position where P1 chooses between the given moves.
    fn pruning_position(moves: [Option<PokemonMove>; 4]) -> BattleState {
        battle_state_from_lists(
            vec![mon(Species::Pikachu, moves)],
            vec![],
            vec![mon(Species::Snorlax, four_moves())],
            vec![],
        )
    }

    /// The move slots that slot 0 still offers.
    fn slot_0_move_slots(joint: &JointActions) -> HashSet<usize> {
        joint
            .actions
            .iter()
            .filter_map(|combo| match &combo[0] {
                BattleCommand::Attack(attack) => Some(attack.move_slot),
                _ => None,
            })
            .collect()
    }

    fn pruned(state: &BattleState, prune: bool) -> JointActions {
        joint_actions(
            state,
            Player::P1,
            Phase::Normal,
            move_dex(),
            pokemon_dex(),
            None,
            prune,
        )
    }

    /// Strength beats Tackle on damage and ties on accuracy, and neither move
    /// carries another effect. Tackle is therefore a proven waste of the turn.
    #[test]
    fn dominated_attack_is_removed_behind_the_flag() {
        let state = pruning_position([
            Some(PokemonMove::Tackle),
            Some(PokemonMove::Strength),
            Some(PokemonMove::Protect),
            Some(PokemonMove::Splash),
        ]);
        let joint = pruned(&state, true);
        let kept = slot_0_move_slots(&joint);
        assert!(!kept.contains(&0), "Tackle survived: {:?}", kept);
        assert!(kept.contains(&1), "Strength disappeared: {:?}", kept);
        // The removal is an approximation, so the caller must hear about it.
        assert!(joint.was_capped());
        for combo in &joint.actions {
            assert!(validate_battle_command_combination(combo));
        }
    }

    /// The flag is off by default, so no current caller loses an action.
    #[test]
    fn the_flag_default_keeps_every_action() {
        let state = pruning_position([
            Some(PokemonMove::Tackle),
            Some(PokemonMove::Strength),
            Some(PokemonMove::Protect),
            Some(PokemonMove::Splash),
        ]);
        let joint = pruned(&state, false);
        assert!(slot_0_move_slots(&joint).contains(&0));
        assert!(!joint.was_capped());
    }

    /// Tackle and Pound deal the same damage with the same accuracy. Rule 7
    /// breaks the tie on the move slot, so exactly one of the pair survives.
    #[test]
    fn equal_moves_keep_one_action() {
        let state = pruning_position([
            Some(PokemonMove::Tackle),
            Some(PokemonMove::Pound),
            Some(PokemonMove::Protect),
            Some(PokemonMove::Splash),
        ]);
        let kept = slot_0_move_slots(&pruned(&state, true));
        assert!(kept.contains(&0), "the lower slot disappeared: {:?}", kept);
        assert!(!kept.contains(&1), "both equal moves survived: {:?}", kept);
    }

    /// A spread move hits the ally slot too, so its value depends on the
    /// partner command. Razor Leaf must survive a stronger single-target move.
    #[test]
    fn a_spread_move_survives_the_filter() {
        let state = pruning_position([
            Some(PokemonMove::Tackle),
            Some(PokemonMove::Strength),
            Some(PokemonMove::RazorLeaf),
            Some(PokemonMove::Splash),
        ]);
        let kept = slot_0_move_slots(&pruned(&state, true));
        assert!(!kept.contains(&0), "Tackle survived: {:?}", kept);
        assert!(kept.contains(&2), "Razor Leaf disappeared: {:?}", kept);
    }

    /// The comparison reads damage and accuracy only. A burn chance is worth
    /// something that the comparison cannot see, so Ember must survive.
    #[test]
    fn a_move_with_a_secondary_effect_survives_the_filter() {
        let state = pruning_position([
            Some(PokemonMove::Tackle),
            Some(PokemonMove::Strength),
            Some(PokemonMove::Ember),
            Some(PokemonMove::Splash),
        ]);
        let kept = slot_0_move_slots(&pruned(&state, true));
        assert!(!kept.contains(&0), "Tackle survived: {:?}", kept);
        assert!(kept.contains(&2), "Ember disappeared: {:?}", kept);
    }

    /// Salt Cure applies residual damage after its direct hit.
    /// Power Gem must not remove that separate effect.
    #[test]
    fn a_name_based_move_effect_survives_the_filter() {
        let state = pruning_position([
            Some(PokemonMove::SaltCure),
            Some(PokemonMove::PowerGem),
            Some(PokemonMove::Protect),
            Some(PokemonMove::Splash),
        ]);

        let kept = slot_0_move_slots(&pruned(&state, true));
        assert!(kept.contains(&0), "Salt Cure disappeared: {:?}", kept);
        assert!(kept.contains(&1), "Power Gem disappeared: {:?}", kept);
    }

    /// Gale Wings changes Gust's priority at full HP.
    /// Strength must not remove an attack in a different priority bracket.
    #[test]
    fn an_effective_priority_change_keeps_both_attacks() {
        let mut state = pruning_position([
            Some(PokemonMove::Gust),
            Some(PokemonMove::Strength),
            Some(PokemonMove::Protect),
            Some(PokemonMove::Splash),
        ]);
        state.p1_active_mons[0].ability = crate::data::ability::Ability::GaleWings;

        let kept = slot_0_move_slots(&pruned(&state, true));
        assert!(kept.contains(&0), "Gust disappeared: {:?}", kept);
        assert!(kept.contains(&1), "Strength disappeared: {:?}", kept);
    }

    /// A critical hit ignores the defensive boosts of the target. Against a
    /// Defense-boosted target the special Power Gem wins the ordinary branch
    /// while the physical Strength wins the critical branch, so neither move is
    /// dominated.
    #[test]
    fn a_critical_hit_branch_keeps_both_attacks() {
        let mut state = pruning_position([
            Some(PokemonMove::Strength),
            Some(PokemonMove::PowerGem),
            Some(PokemonMove::Protect),
            Some(PokemonMove::Splash),
        ]);
        // Boost index 1 is Defense.
        state.p2_active_mons[0].boosts[1] = 6;

        let kept = slot_0_move_slots(&pruned(&state, true));
        assert!(kept.contains(&0), "Strength disappeared: {:?}", kept);
        assert!(kept.contains(&1), "Power Gem disappeared: {:?}", kept);
    }

    /// `RootSeed` carries action indices between deepening passes, so the
    /// filter must build the same list on every pass over one position.
    #[test]
    fn the_filter_is_deterministic() {
        let state = pruning_position([
            Some(PokemonMove::Tackle),
            Some(PokemonMove::Strength),
            Some(PokemonMove::Ember),
            Some(PokemonMove::RazorLeaf),
        ]);
        let first = pruned(&state, true);
        for _ in 0..5 {
            let again = pruned(&state, true);
            assert_eq!(
                format!("{:?}", first.actions),
                format!("{:?}", again.actions)
            );
        }
    }

    /// A replacement offers switches only, and the filter compares attacks.
    /// It must leave every replacement choice in place.
    #[test]
    fn the_filter_leaves_a_replacement_alone() {
        let mut state = battle_state_from_lists(
            vec![mon(Species::Pikachu, four_moves())],
            vec![
                mon(Species::Snorlax, four_moves()),
                mon(Species::Gengar, four_moves()),
            ],
            vec![mon(Species::Pikachu, four_moves())],
            vec![],
        );
        state.p1_active_mons[0].hp = 0;
        state.p1_active_mons[0].fainted = true;
        state.turn_started = true;
        state.turn_ended = true;

        let joint = joint_actions(
            &state,
            Player::P1,
            Phase::Replacement,
            move_dex(),
            pokemon_dex(),
            None,
            true,
        );
        assert_eq!(joint.actions.len(), 2);
        assert!(!joint.was_capped());
    }
}
