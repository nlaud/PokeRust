//! Engine state → DTO conversion. All display strings are produced here so the
//! frontend never sees raw enum debug output it didn't ask for.

use crate::dto::*;
use poke_rust::data::item::Item;
use poke_rust::information::information::{CantReason, EventKind, InformationEvent, SwitchState};
use poke_rust::information::unknowns::PokemonHP;
use poke_rust::state::battle::{BattleCommand, BattleState, FieldSlot, MatchState, Player, TeamPreviewState};
use poke_rust::state::dex_data::{SideCondition, SlotCondition, Status, VolatileStatus};
use poke_rust::state::pokemon::{PokemonState, VolatileStatusState};
use poke_rust::user::{battle_command_description, back_mon_name, humanize_identifier, move_name};

pub fn player_dto(player: Player) -> PlayerDto {
    match player {
        Player::P1 => PlayerDto::P1,
        Player::P2 => PlayerDto::P2,
    }
}

pub fn player_from_dto(player: PlayerDto) -> Player {
    match player {
        PlayerDto::P1 => Player::P1,
        PlayerDto::P2 => Player::P2,
    }
}

pub fn field_slot_dto(slot: FieldSlot) -> FieldSlotDto {
    FieldSlotDto {
        player: player_dto(slot.player),
        slot_index: slot.slot_index,
    }
}

pub fn field_slot_from_dto(slot: FieldSlotDto) -> FieldSlot {
    FieldSlot {
        player: player_from_dto(slot.player),
        slot_index: slot.slot_index,
    }
}

fn observed_hp_dto(hp: &PokemonHP) -> ObservedHpDto {
    match hp {
        PokemonHP::Number(n) => ObservedHpDto {
            exact: Some(*n),
            percent: None,
        },
        PokemonHP::Percent(p) => ObservedHpDto {
            exact: None,
            percent: Some(*p),
        },
    }
}

pub fn status_dto(status: &Status) -> StatusDto {
    let (code, turns) = match status {
        Status::Burn => ("BRN", None),
        Status::Poison => ("PSN", None),
        Status::ToxicPoison(n) => ("TOX", Some(*n)),
        Status::Paralysis => ("PAR", None),
        Status::Sleep(n) => ("SLP", Some(*n)),
        Status::Frozen(n) => ("FRZ", Some(*n)),
    };
    StatusDto {
        code: code.to_string(),
        turns,
    }
}

fn volatile_name(volatile: &VolatileStatus) -> String {
    match volatile {
        VolatileStatus::Disable(m) => format!("Disable ({})", move_name(m)),
        VolatileStatus::Encore(m) => format!("Encore ({})", move_name(m)),
        VolatileStatus::ChoiceLock(m) => format!("Choice Lock ({})", move_name(m)),
        VolatileStatus::CantUseRepeatedly(m) => format!("Can't Repeat ({})", move_name(m)),
        VolatileStatus::LockedMove(m) => format!("Locked Move ({})", move_name(m)),
        VolatileStatus::SemiInvulnerable(m) => format!("Semi-Invulnerable ({})", move_name(m)),
        VolatileStatus::Substitute(hp) => format!("Substitute ({} HP)", hp),
        VolatileStatus::Stockpile(n) => format!("Stockpile {}", n),
        VolatileStatus::SupremeOverlord(n) => format!("Supreme Overlord ({})", n),
        other => humanize_identifier(format!("{:?}", other)),
    }
}

fn volatile_dto(volatile: &VolatileStatusState) -> VolatileDto {
    match volatile {
        VolatileStatusState::TurnStatus(v, turns) | VolatileStatusState::MoveStatus(v, turns) => {
            VolatileDto {
                name: volatile_name(v),
                turns: if *turns > 0 { Some(*turns) } else { None },
            }
        }
        VolatileStatusState::Charging(m, _) => VolatileDto {
            name: format!("Charging {}", move_name(m)),
            turns: None,
        },
    }
}

fn item_name(item: &Item) -> Option<String> {
    if *item == Item::None {
        None
    } else {
        Some(humanize_identifier(format!("{:?}", item)))
    }
}

fn side_condition_name(condition: &SideCondition) -> String {
    match condition {
        SideCondition::Spikes(layers) => format!("Spikes ({})", layers),
        SideCondition::ToxicSpikes(layers) => format!("Toxic Spikes ({})", layers),
        SideCondition::StickyWeb(_) => "Sticky Web".to_string(),
        other => humanize_identifier(format!("{:?}", other)),
    }
}

fn slot_condition_name(condition: &SlotCondition) -> String {
    match condition {
        SlotCondition::FutureMove { move_name: m, .. } => {
            format!("{} (incoming)", move_name(m))
        }
        SlotCondition::Wish { .. } => "Wish".to_string(),
        other => humanize_identifier(format!("{:?}", other)),
    }
}

pub fn pokemon_view(mon: &PokemonState) -> PokemonView {
    PokemonView {
        mon_id: mon.mon_id,
        species: mon.species.to_string(),
        level: mon.level,
        gender: format!("{:?}", mon.gender),
        types: mon.types.iter().map(|t| format!("{:?}", t)).collect(),
        hp: HpDto {
            current: mon.hp,
            max: mon.stats[0],
        },
        fainted: mon.fainted,
        status: mon.status.as_ref().map(status_dto),
        volatiles: mon.volatiles.iter().map(volatile_dto).collect(),
        stats: mon.stats,
        boosts: mon.boosts,
        nature: format!("{:?}", mon.nature),
        item: item_name(&mon.item),
        ability: mon.ability.to_string(),
        moves: (0..4)
            .map(|i| {
                mon.moves[i].as_ref().map(|m| MoveViewDto {
                    name: m.to_string(),
                    pp: mon.move_pp[i],
                    max_pp: mon.max_pp[i],
                })
            })
            .collect(),
        is_tera: mon.is_tera,
        tera_type: format!("{:?}", mon.tera_type),
        is_mega: mon.is_mega,
    }
}

fn named_turns(name: String, turns: Option<u8>) -> NamedTurnsDto {
    NamedTurnsDto { name, turns }
}

fn side_view(state: &BattleState, player: Player) -> SideView {
    let (active, back, can_tera, can_mega, conditions, condition_turns, slot_conditions) =
        match player {
            Player::P1 => (
                &state.p1_active_mons,
                &state.p1_back_mons,
                state.p1_has_tera,
                state.p1_has_mega,
                &state.p1_side_conditions,
                &state.p1_side_condition_turns,
                &state.p1_slot_conditions,
            ),
            Player::P2 => (
                &state.p2_active_mons,
                &state.p2_back_mons,
                state.p2_has_tera,
                state.p2_has_mega,
                &state.p2_side_conditions,
                &state.p2_side_condition_turns,
                &state.p2_slot_conditions,
            ),
        };

    SideView {
        active: active.iter().map(pokemon_view).collect(),
        back: back.iter().map(pokemon_view).collect(),
        can_tera,
        can_mega,
        side_conditions: conditions
            .iter()
            .zip(condition_turns.iter())
            .map(|(c, t)| named_turns(side_condition_name(c), Some(*t)))
            .collect(),
        slot_conditions: slot_conditions
            .iter()
            .map(|conds| conds.iter().map(slot_condition_name).collect())
            .collect(),
    }
}

fn field_view(state: &BattleState) -> FieldView {
    FieldView {
        weather: state
            .weather
            .as_ref()
            .map(|w| named_turns(humanize_identifier(format!("{:?}", w)), state.weather_turns)),
        terrain: state
            .terrain
            .as_ref()
            .map(|t| named_turns(humanize_identifier(format!("{:?}", t)), state.terrain_turns)),
        pseudo_weathers: state
            .pseudo_weathers
            .iter()
            .zip(state.pseudo_weather_turns.iter())
            .map(|(pw, t)| named_turns(humanize_identifier(format!("{:?}", pw)), Some(*t)))
            .collect(),
    }
}

/// Which input phase the battle is in — the same dispatch the terminal driver uses
/// in `user::choose_battle_commands_for_player`.
pub fn phase_of(state: &MatchState) -> PhaseDto {
    match state {
        MatchState::TeamPreviewState(_) => PhaseDto::TeamPreview,
        MatchState::GameOverState { .. } => PhaseDto::GameOver,
        MatchState::BattleState(battle) => {
            if battle.self_switch_pending.is_some() {
                PhaseDto::SelfSwitch
            } else if battle.turn_started && battle.turn_ended {
                PhaseDto::Replacement
            } else {
                PhaseDto::Normal
            }
        }
    }
}

fn preview_view(preview: &TeamPreviewState) -> PreviewView {
    PreviewView {
        active_per_side: preview.active_per_side,
        brought_per_side: preview.brought_per_side,
        p1_mons: preview.p1_mons.iter().map(pokemon_view).collect(),
        p2_mons: preview.p2_mons.iter().map(pokemon_view).collect(),
    }
}

pub fn battle_view(state: &MatchState, active_per_side: u8, brought_per_side: u8) -> BattleView {
    let phase = phase_of(state);
    let mut view = BattleView {
        phase,
        turn_number: 0,
        active_per_side,
        brought_per_side,
        preview: None,
        p1: None,
        p2: None,
        field: None,
        self_switch: None,
        winner: None,
    };

    match state {
        MatchState::TeamPreviewState(preview) => {
            view.preview = Some(preview_view(preview));
        }
        MatchState::BattleState(battle) => {
            view.turn_number = battle.turn_number;
            view.p1 = Some(side_view(battle, Player::P1));
            view.p2 = Some(side_view(battle, Player::P2));
            view.field = Some(field_view(battle));
            view.self_switch = battle
                .self_switch_pending
                .map(|(slot, _)| field_slot_dto(slot));
        }
        MatchState::GameOverState { winner } => {
            view.winner = Some(player_dto(*winner));
        }
    }

    view
}

// ── Commands ─────────────────────────────────────────────────────────────────

pub fn battle_command_dto(command: &BattleCommand) -> BattleCommandDto {
    match command {
        BattleCommand::Attack(attack) => BattleCommandDto::Attack {
            move_slot: attack.move_slot,
            target: attack.target.map(field_slot_dto),
            terastallize: attack.terastallize,
            mega_evolve: attack.mega_evolve,
        },
        BattleCommand::Switch(switch) => BattleCommandDto::Switch {
            party_index: switch.party_index,
        },
        BattleCommand::Struggle { target } => BattleCommandDto::Struggle {
            target: target.map(field_slot_dto),
        },
        BattleCommand::Pass => BattleCommandDto::Pass,
    }
}

pub fn battle_command_from_dto(dto: &BattleCommandDto) -> BattleCommand {
    match dto {
        BattleCommandDto::Attack {
            move_slot,
            target,
            terastallize,
            mega_evolve,
        } => BattleCommand::Attack(poke_rust::state::battle::AttackCommand {
            move_slot: *move_slot,
            target: target.map(field_slot_from_dto),
            terastallize: *terastallize,
            mega_evolve: *mega_evolve,
        }),
        BattleCommandDto::Switch { party_index } => {
            BattleCommand::Switch(poke_rust::state::battle::SwitchCommand {
                party_index: *party_index,
            })
        }
        BattleCommandDto::Struggle { target } => BattleCommand::Struggle {
            target: target.map(field_slot_from_dto),
        },
        BattleCommandDto::Pass => BattleCommand::Pass,
    }
}

/// Short label for a command button: the move name for attacks, the incoming
/// Pokémon for switches.
fn command_label(state: &BattleState, player: Player, slot_idx: usize, command: &BattleCommand) -> Option<String> {
    let active_mon = match player {
        Player::P1 => state.p1_active_mons.get(slot_idx),
        Player::P2 => state.p2_active_mons.get(slot_idx),
    };
    match command {
        BattleCommand::Attack(attack) => active_mon
            .and_then(|mon| mon.moves.get(attack.move_slot).and_then(|m| m.as_ref()))
            .map(|m| m.to_string()),
        BattleCommand::Switch(switch) => Some(back_mon_name(state, player, switch.party_index)),
        BattleCommand::Struggle { .. } => Some("Struggle".to_string()),
        BattleCommand::Pass => None,
    }
}

pub fn command_option(
    state: &BattleState,
    player: Player,
    slot_idx: usize,
    command: &BattleCommand,
) -> CommandOptionDto {
    CommandOptionDto {
        command: battle_command_dto(command),
        description: battle_command_description(state, player, slot_idx, command),
        label: command_label(state, player, slot_idx, command),
    }
}

// ── Events ───────────────────────────────────────────────────────────────────

fn switch_dto(switch: &SwitchState) -> SwitchDto {
    SwitchDto {
        slot: field_slot_dto(switch.slot),
        species: switch.species.to_string(),
        level: switch.level,
        hp: observed_hp_dto(&switch.hp),
        status: switch.status.as_ref().map(status_dto),
        tera_type: switch.tera_type.as_ref().map(|t| format!("{:?}", t)),
    }
}

fn cant_reason_name(reason: &CantReason) -> String {
    humanize_identifier(format!("{:?}", reason))
}

const BOOST_NAMES: [&str; 7] = ["Atk", "Def", "SpA", "SpD", "Spe", "Acc", "Eva"];

pub fn event_node(event: &InformationEvent) -> EventNode {
    EventNode {
        kind: event_kind_dto(&event.kind),
        reactions: event.reactions.iter().map(event_node).collect(),
    }
}

fn event_kind_dto(kind: &EventKind) -> EventKindDto {
    match kind {
        EventKind::MoveUsed {
            user,
            move_used,
            targets,
        } => EventKindDto::MoveUsed {
            user: field_slot_dto(*user),
            r#move: move_used.to_string(),
            targets: targets.iter().copied().map(field_slot_dto).collect(),
        },
        EventKind::Switch(switch) => EventKindDto::Switch {
            switch: switch_dto(switch),
        },
        EventKind::SimultaneousSwitch { switches } => EventKindDto::SimultaneousSwitch {
            switches: switches.iter().map(switch_dto).collect(),
        },
        EventKind::EndOfTurn => EventKindDto::EndOfTurn,
        EventKind::Faint { slot } => EventKindDto::Faint {
            slot: field_slot_dto(*slot),
        },
        EventKind::MegaEvolution { slot, into } => EventKindDto::MegaEvolution {
            slot: field_slot_dto(*slot),
            into: into.to_string(),
        },
        EventKind::Terastallization { slot, tera_type } => EventKindDto::Terastallization {
            slot: field_slot_dto(*slot),
            tera_type: format!("{:?}", tera_type),
        },
        EventKind::FormeChange {
            slot,
            into,
            permanent,
        } => EventKindDto::FormeChange {
            slot: field_slot_dto(*slot),
            into: into.to_string(),
            permanent: *permanent,
        },
        EventKind::TypeChanged { slot, new_types } => EventKindDto::TypeChanged {
            slot: field_slot_dto(*slot),
            new_types: new_types.iter().map(|t| format!("{:?}", t)).collect(),
        },
        EventKind::Cant { slot, reason } => EventKindDto::Cant {
            slot: field_slot_dto(*slot),
            reason: cant_reason_name(reason),
        },
        EventKind::ChargingMove { user, move_used } => EventKindDto::ChargingMove {
            user: field_slot_dto(*user),
            r#move: move_used.to_string(),
        },
        EventKind::MustRecharge { slot } => EventKindDto::MustRecharge {
            slot: field_slot_dto(*slot),
        },
        EventKind::SingleMoveOrTurn { slot, move_used } => EventKindDto::SingleMoveOrTurn {
            slot: field_slot_dto(*slot),
            r#move: move_used.to_string(),
        },
        EventKind::DamageDealt { target, new_hp } => EventKindDto::DamageDealt {
            target: field_slot_dto(*target),
            new_hp: observed_hp_dto(new_hp),
        },
        EventKind::Healed { target, new_hp } => EventKindDto::Healed {
            target: field_slot_dto(*target),
            new_hp: observed_hp_dto(new_hp),
        },
        EventKind::SetHp { target, new_hp } => EventKindDto::SetHp {
            target: field_slot_dto(*target),
            new_hp: observed_hp_dto(new_hp),
        },
        EventKind::Crit { target } => EventKindDto::Crit {
            target: field_slot_dto(*target),
        },
        EventKind::Immune { target } => EventKindDto::Immune {
            target: field_slot_dto(*target),
        },
        EventKind::Missed { target } => EventKindDto::Missed {
            target: field_slot_dto(*target),
        },
        EventKind::MoveFailed { slot } => EventKindDto::MoveFailed {
            slot: field_slot_dto(*slot),
        },
        EventKind::Blocked { target } => EventKindDto::Blocked {
            target: field_slot_dto(*target),
        },
        EventKind::HitCount { target, hits } => EventKindDto::HitCount {
            target: field_slot_dto(*target),
            hits: *hits,
        },
        EventKind::StatusInflicted { target, status } => EventKindDto::StatusInflicted {
            target: field_slot_dto(*target),
            status: status_dto(status),
        },
        EventKind::StatusCured { target, status } => EventKindDto::StatusCured {
            target: field_slot_dto(*target),
            status: status_dto(status),
        },
        EventKind::TeamStatusCured { side } => EventKindDto::TeamStatusCured {
            side: player_dto(*side),
        },
        EventKind::BoostChanged {
            target,
            boost_idx,
            stages,
        } => EventKindDto::BoostChanged {
            target: field_slot_dto(*target),
            stat: BOOST_NAMES
                .get(*boost_idx)
                .copied()
                .unwrap_or("?")
                .to_string(),
            stages: *stages,
        },
        EventKind::BoostsCleared { target } => EventKindDto::BoostsCleared {
            target: field_slot_dto(*target),
        },
        EventKind::BoostsInverted { target } => EventKindDto::BoostsInverted {
            target: field_slot_dto(*target),
        },
        EventKind::BoostsSwapped { source, target } => EventKindDto::BoostsSwapped {
            source: field_slot_dto(*source),
            target: field_slot_dto(*target),
        },
        EventKind::BoostsCopied { source, target } => EventKindDto::BoostsCopied {
            source: field_slot_dto(*source),
            target: field_slot_dto(*target),
        },
        EventKind::WeatherChanged { weather } => EventKindDto::WeatherChanged {
            weather: weather
                .as_ref()
                .map(|w| humanize_identifier(format!("{:?}", w))),
        },
        EventKind::TerrainChanged { terrain } => EventKindDto::TerrainChanged {
            terrain: terrain
                .as_ref()
                .map(|t| humanize_identifier(format!("{:?}", t))),
        },
        EventKind::PseudoWeatherStart { effect } => EventKindDto::PseudoWeatherStart {
            effect: humanize_identifier(format!("{:?}", effect)),
        },
        EventKind::PseudoWeatherEnd { effect } => EventKindDto::PseudoWeatherEnd {
            effect: humanize_identifier(format!("{:?}", effect)),
        },
        EventKind::SideConditionStart { side, condition } => EventKindDto::SideConditionStart {
            side: player_dto(*side),
            condition: side_condition_name(condition),
        },
        EventKind::SideConditionEnd { side, condition } => EventKindDto::SideConditionEnd {
            side: player_dto(*side),
            condition: side_condition_name(condition),
        },
        EventKind::SlotConditionStart { slot, condition } => EventKindDto::SlotConditionStart {
            slot: field_slot_dto(*slot),
            condition: slot_condition_name(condition),
        },
        EventKind::SlotConditionEnd { slot, condition } => EventKindDto::SlotConditionEnd {
            slot: field_slot_dto(*slot),
            condition: slot_condition_name(condition),
        },
        EventKind::VolatileStart { target, volatile } => EventKindDto::VolatileStart {
            target: field_slot_dto(*target),
            volatile: volatile_name(volatile),
        },
        EventKind::VolatileEnd { target, volatile } => EventKindDto::VolatileEnd {
            target: field_slot_dto(*target),
            volatile: volatile_name(volatile),
        },
        EventKind::PerishCount { target, turns_left } => EventKindDto::PerishCount {
            target: field_slot_dto(*target),
            turns_left: *turns_left,
        },
        EventKind::ItemRevealed { slot, item } => EventKindDto::ItemRevealed {
            slot: field_slot_dto(*slot),
            item: item_name(item).unwrap_or_else(|| "None".to_string()),
        },
        EventKind::ItemGained { slot, item } => EventKindDto::ItemGained {
            slot: field_slot_dto(*slot),
            item: item_name(item).unwrap_or_else(|| "None".to_string()),
        },
        EventKind::ItemLost {
            slot,
            item,
            consumed,
        } => EventKindDto::ItemLost {
            slot: field_slot_dto(*slot),
            item: item_name(item).unwrap_or_else(|| "None".to_string()),
            consumed: *consumed,
        },
        EventKind::AbilityRevealed { slot, ability } => EventKindDto::AbilityRevealed {
            slot: field_slot_dto(*slot),
            ability: ability.to_string(),
        },
        EventKind::AnticipationShudder { slot } => EventKindDto::AnticipationShudder {
            slot: field_slot_dto(*slot),
        },
        EventKind::IllusionEnded {
            slot,
            actual_species,
        } => EventKindDto::IllusionEnded {
            slot: field_slot_dto(*slot),
            actual_species: actual_species.to_string(),
        },
        EventKind::Transformed {
            slot,
            into_slot,
            into_species,
        } => EventKindDto::Transformed {
            slot: field_slot_dto(*slot),
            into_slot: field_slot_dto(*into_slot),
            into_species: into_species.to_string(),
        },
    }
}
