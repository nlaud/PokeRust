//! Engine state → DTO conversion. All display strings are produced here so the
//! frontend never sees raw enum debug output it didn't ask for.

use crate::dto::*;
use poke_rust::data::item::Item;
use poke_rust::information::describe::{
    describe_clause, describe_move_slot, describe_unknown, describe_unknown_item,
};
use poke_rust::information::information::{CantReason, EventKind, InformationEvent, SwitchState};
use poke_rust::information::unknowns::{
    PokemonHP, Unknown, UnknownBattleState, UnknownMatchState, UnknownPokemonState,
};
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
        // The variant is spelled TailWind; the move is one word.
        SideCondition::TailWind => "Tailwind".to_string(),
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
        // A real player always sees the physically-displayed appearance — the
        // Illusion disguise when one is active — never the true species underneath,
        // regardless of information mode (this is a visual fact, not secret
        // team-sheet info). `mon.types` still reflects the TRUE species since it
        // drives damage calc; a fully faithful disguised-type display would need a
        // dex lookup this function doesn't have, so it's left as a known gap.
        species: mon
            .illusion_disguise
            .as_ref()
            .map(|s| s.to_string())
            .unwrap_or_else(|| mon.species.to_string()),
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
        stats_max: mon.stats,
        boosts: mon.boosts,
        nature: format!("{:?}", mon.nature),
        evs: mon.evs,
        evs_max: mon.evs,
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
        is_illusion_suspected: false,
    }
}

/// Overlay belief-derived masking onto an otherwise-ground-truth `PokemonView`: the
/// fields a real open team sheet (or a real player's screen) keeps secret — nature,
/// EVs/stats-as-ranges, item, ability, unrevealed moves, a pre-reveal Tera type, exact
/// HP, and typing while a species disguise is unresolved — are replaced. Status,
/// volatiles, boosts, the (already Illusion-aware) species/sprite, gender, fainted,
/// isTera/isMega are directly observable in a real battle regardless of information
/// mode, so those stay ground truth.
fn mask_pokemon_view(mut view: PokemonView, unk: &UnknownPokemonState) -> PokemonView {
    view.nature = describe_unknown(&unk.possible_natures);
    view.stats = unk.minStats;
    view.stats_max = unk.maxStats;
    view.evs = unk.minEvs;
    view.evs_max = unk.maxEvs;
    // A real player only ever sees the opponent's HP as a rounded percent, never the
    // exact value — mirror `bench_pokemon_view_from_belief`'s conversion (percent
    // scaled against the believed max HP) rather than leaving `view.hp` on the true
    // `mon.hp`/`mon.stats[0]` set by `pokemon_view`. `unk.hp` is `Percent` for every
    // opponent mon in practice (see `PokemonHP`'s doc); `Number` is handled defensively
    // for parity with the bench helper.
    let masked_max_hp = unk.maxStats[0];
    view.hp = HpDto {
        current: match &unk.hp {
            PokemonHP::Number(n) => *n,
            PokemonHP::Percent(p) => ((*p as u32 * masked_max_hp as u32) / 100) as u16,
        },
        max: masked_max_hp,
    };
    // Typing is public dex knowledge for a normal opponent (`possible_types` is
    // already `Known` from `from_opponent_species`, so this is a no-op there), but for
    // a suspected Illusion disguise `possible_types` widens to `Possibly` alongside
    // `possible_species` (see `maybe_widen_for_illusion`) — mirror
    // `bench_pokemon_view_from_belief`'s treatment so a disguised Zoroark's row shows
    // unknown typing instead of the true species' `mon.types` `pokemon_view` set.
    view.types = match &unk.possible_types {
        Unknown::Known(types) => types.iter().map(|t| format!("{:?}", t)).collect(),
        _ => Vec::new(),
    };
    view.item = match &unk.item {
        Unknown::Known(Item::None) => None,
        // `legal_items` (format banlists) isn't threaded to the server yet — see
        // `InferenceConfig` construction — so this always renders against the
        // unrestricted (~1,000-item) pool.
        other => Some(describe_unknown_item(other, None)),
    };
    view.ability = describe_unknown(&unk.possible_abilities);
    view.moves = (0..4)
        .map(|i| {
            Some(MoveViewDto {
                name: describe_move_slot(unk.known_moves[i].clone()),
                pp: unk.move_pp[i].max(0) as u8,
                max_pp: unk.max_pp[i].max(0) as u8,
            })
        })
        .collect();
    // A pre-reveal Tera type is genuinely secret in a real battle too (the Tera
    // Orb icon only shows it once activated) — mask it until `is_tera` flips true.
    if !view.is_tera {
        view.tera_type = describe_unknown(&unk.possible_tera_type);
    }
    view.is_illusion_suspected =
        matches!(&unk.possible_species, Unknown::Possibly(c) if c.len() > 1);
    view
}

/// Build a `PokemonView` for a benched Pokémon from the belief alone (no reliable
/// concrete `PokemonState` pairing exists for bench mons — the inference engine's
/// own bench bookkeeping doesn't preserve list order against `BattleState`'s). Boosts
/// and volatiles are exactly `[0;7]`/empty for any benched mon (both reset on
/// switch-out), so this is not an approximation for those two fields; HP is
/// approximated from the believed max HP when only a percent is known.
///
/// `mon_id` prefers the belief's own `possible_mon_id` (narrowed to `Known` once the
/// party-order slot is pinned down); when it's still ambiguous, falls back to
/// `fallback_id` — a caller-supplied id that must be unique across the whole side's
/// bench render for this call (see `side_view`). Without this, every unresolved bench
/// mon rendered `mon_id: 0` and collided on the frontend's `mon_id`-keyed rows.
fn bench_pokemon_view_from_belief(unk: &UnknownPokemonState, fallback_id: u8) -> PokemonView {
    let max_hp = unk.maxStats[0];
    let current = match unk.hp {
        PokemonHP::Number(n) => n,
        PokemonHP::Percent(p) => ((p as u32 * max_hp as u32) / 100) as u16,
    };
    let mon_id = match unk.possible_mon_id {
        Unknown::Known(id) => id,
        _ => fallback_id,
    };
    let mut view = PokemonView {
        mon_id,
        species: describe_unknown(&unk.possible_species),
        level: unk.level,
        gender: describe_unknown(&unk.possible_genders),
        types: match &unk.possible_types {
            Unknown::Known(types) => types.iter().map(|t| format!("{:?}", t)).collect(),
            _ => Vec::new(),
        },
        hp: HpDto { current, max: max_hp },
        fainted: unk.fainted,
        status: unk.status.as_ref().map(status_dto),
        volatiles: unk.volatiles.iter().map(volatile_dto).collect(),
        stats: unk.minStats,
        stats_max: unk.maxStats,
        boosts: [0; 7],
        nature: describe_unknown(&unk.possible_natures),
        evs: unk.minEvs,
        evs_max: unk.maxEvs,
        item: None,
        ability: describe_unknown(&unk.possible_abilities),
        moves: Vec::new(),
        is_tera: unk.is_tera,
        tera_type: describe_unknown(&unk.possible_tera_type),
        is_mega: unk.is_mega,
        is_illusion_suspected: false,
    };
    view.item = match &unk.item {
        Unknown::Known(Item::None) => None,
        other => Some(describe_unknown_item(other, None)),
    };
    view.moves = (0..4)
        .map(|i| {
            Some(MoveViewDto {
                name: describe_move_slot(unk.known_moves[i].clone()),
                pp: unk.move_pp[i].max(0) as u8,
                max_pp: unk.max_pp[i].max(0) as u8,
            })
        })
        .collect();
    view
}

fn named_turns(name: String, turns: Option<u8>) -> NamedTurnsDto {
    NamedTurnsDto { name, turns }
}

/// The belief's battle-phase fog state for `player`, when one is being tracked and
/// has already transitioned past team preview. `None` under Perfect Information, or
/// (defensively) if the belief hasn't reached the `Battle` variant yet — masking is
/// display-only and must never panic, so this just falls back to ground truth.
fn belief_battle_state(belief: Option<&UnknownMatchState>) -> Option<&UnknownBattleState> {
    match belief {
        Some(UnknownMatchState::Battle(b)) => Some(b),
        _ => None,
    }
}

fn side_view(state: &BattleState, player: Player, belief: Option<&UnknownMatchState>) -> SideView {
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

    // Only P2 (the opponent) is ever masked — P1 is the viewer's own team, always
    // fully known. `active` is zipped by index with the belief's active mons (both
    // stay in lockstep, actives-first, throughout the battle); known/possible back
    // mons are rendered straight from the belief alone (see
    // `bench_pokemon_view_from_belief`'s doc comment for why no concrete pairing is
    // attempted there).
    let fog = if player == Player::P2 { belief_battle_state(belief) } else { None };

    let (active_views, back_views, possible_back_views) = match fog {
        Some(fog) => {
            let active_views: Vec<PokemonView> = active
                .iter()
                .enumerate()
                .map(|(i, mon)| {
                    let base = pokemon_view(mon);
                    match fog.p2_active_mons.get(i) {
                        Some(unk) => mask_pokemon_view(base, unk),
                        None => base,
                    }
                })
                .collect();
            // Fallback ids for mons whose `possible_mon_id` hasn't narrowed to `Known`
            // yet: real party-order ids only ever range 0..=5, so offsetting each
            // section's fallback base well above that (and apart from each other)
            // guarantees no two bench rows ever collide on `mon_id` — see
            // `bench_pokemon_view_from_belief`'s doc comment.
            let back_views: Vec<PokemonView> = fog
                .p2_known_back_mons
                .iter()
                .enumerate()
                .map(|(i, unk)| bench_pokemon_view_from_belief(unk, 100 + i as u8))
                .collect();
            let possible_back_views: Vec<PokemonView> = fog
                .p2_possible_back_mons
                .iter()
                .enumerate()
                .map(|(i, unk)| bench_pokemon_view_from_belief(unk, 150 + i as u8))
                .collect();
            (active_views, back_views, possible_back_views)
        }
        None => (
            active.iter().map(pokemon_view).collect(),
            back.iter().map(pokemon_view).collect(),
            Vec::new(),
        ),
    };

    SideView {
        active: active_views,
        back: back_views,
        possible_back: possible_back_views,
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

fn preview_view(preview: &TeamPreviewState, belief: Option<&UnknownMatchState>) -> PreviewView {
    // Mirrors `side_view`: only P2 is ever masked, zipped by index with the
    // belief's team-preview mon list (both built from the same species list in the
    // same order — see `team_preview_open_sheet_from_perspective`).
    let fog_p2_mons: Option<&[UnknownPokemonState]> = match belief {
        Some(UnknownMatchState::TeamPreview(fog)) => Some(&fog.p2_mons),
        _ => None,
    };
    let p2_mons: Vec<PokemonView> = preview
        .p2_mons
        .iter()
        .enumerate()
        .map(|(i, mon)| {
            let base = pokemon_view(mon);
            match fog_p2_mons.and_then(|mons| mons.get(i)) {
                Some(unk) => mask_pokemon_view(base, unk),
                None => base,
            }
        })
        .collect();

    PreviewView {
        active_per_side: preview.active_per_side,
        brought_per_side: preview.brought_per_side,
        p1_mons: preview.p1_mons.iter().map(pokemon_view).collect(),
        p2_mons,
    }
}

pub fn battle_view(
    state: &MatchState,
    active_per_side: u8,
    brought_per_side: u8,
    belief: Option<&UnknownMatchState>,
) -> BattleView {
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
        belief: None,
    };

    match state {
        MatchState::TeamPreviewState(preview) => {
            view.preview = Some(preview_view(preview, belief));
        }
        MatchState::BattleState(battle) => {
            view.turn_number = battle.turn_number;
            view.p1 = Some(side_view(battle, Player::P1, belief));
            view.p2 = Some(side_view(battle, Player::P2, belief));
            view.field = Some(field_view(battle));
            view.self_switch = battle
                .self_switch_pending
                .map(|(slot, _)| field_slot_dto(slot));
            view.belief = belief_battle_state(belief).map(|fog| BeliefView {
                clauses: fog.predicates.iter().map(|clause| describe_clause(clause, fog)).collect(),
            });
        }
        MatchState::GameOverState { winner, final_state, .. } => {
            view.winner = Some(player_dto(*winner));
            // Show the field as it stood when the battle ended (fainted mon,
            // final HP) behind the winner overlay.
            view.turn_number = final_state.turn_number;
            view.p1 = Some(side_view(final_state, Player::P1, belief));
            view.p2 = Some(side_view(final_state, Player::P2, belief));
            view.field = Some(field_view(final_state));
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
