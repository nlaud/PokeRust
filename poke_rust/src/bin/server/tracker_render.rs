//! Converts a masked engine event stream to tracker text.
//! Round-trip tests use this module to check tracker accuracy.
//!
//! # Scope
//!
//! The tracker grammar does not represent every `EventKind`.
//! The renderer supports common moves, switches, field effects, abilities, items, Mega Evolution, and Terastallization.
//! It returns `Err` for unsupported events.
//! The randomized test corpus contains only supported events.
//!
//! # Avoiding double-application
//!
//! Users do not enter guaranteed effects.
//! `tracker_effects::augment_turn` adds them after parsing.
//! The renderer must omit these effects to prevent duplicate application.
//! `reactions_requiring_explicit_render` compares actual reactions with generated reactions.
//! It renders only reactions that the generator cannot add.
//!
//! No HTTP route uses this module.
//! It supports the hand-written and randomized tracker tests.
//! Normal builds therefore permit unused code here.
#![allow(dead_code)]

use std::collections::HashMap;
use std::fmt::Write as _;

use poke_rust::data::ability::Ability;
use poke_rust::data::item::Item;
use poke_rust::data::pokemon_move::PokemonMove;
use poke_rust::data::species::Species;
use poke_rust::information::information::{EventKind, InformationEvent, SwitchState};
use poke_rust::information::unknowns::{PokemonHP, UnknownBattleState};
use poke_rust::state::battle::{FieldSlot, Player};
use poke_rust::state::dex_data::{
    MoveData, PokemonData, PokemonType, Status, Terrain, VolatileStatus, Weather,
};

use crate::tracker_effects::{
    augment_with_guaranteed_effects, fold_event_into_synthesis_scratch,
};

/// A reaction/event this renderer has no tracker-grammar word for. Carries a
/// short human-readable reason so a fuzz failure identifies the missing grammar.
#[derive(Debug)]
pub struct Unsupported(pub String);

fn unsupported(reason: impl Into<String>) -> Unsupported {
    Unsupported(reason.into())
}

/// Render one whole turn's already-masked events into tracker-syntax text
/// (newline-separated lines, `endofturn`-terminated) — the exact submission
/// shape `POST /api/tracker/{id}/events` expects. `belief` must be the
/// tracker's committed belief from BEFORE this turn (mirrors
/// `augment_with_guaranteed_effects`'s own `belief` contract).
pub fn render_turn(
    events: &[InformationEvent],
    belief: &UnknownBattleState,
    move_dex: &HashMap<PokemonMove, MoveData>,
    pokemon_dex: &HashMap<Species, PokemonData>,
) -> Result<String, Unsupported> {
    let mut lines = Vec::new();
    let mut scratch = belief.clone();
    for event in events {
        if matches!(event.kind, EventKind::EndOfTurn) {
            let mut reaction_lines = Vec::new();
            for reaction in &event.reactions {
                render_standalone_tree(reaction, &scratch, &mut reaction_lines)?;
            }
            for line in reaction_lines {
                lines.push(format!("eot {line}"));
            }
            fold_event_into_synthesis_scratch(&mut scratch, event, pokemon_dex);
            continue; // the sentinel itself is added once below.
        }
        if let Some(line) = render_top_level_event(event, &scratch, move_dex, pokemon_dex)? {
            lines.push(line);
        }
        fold_event_into_synthesis_scratch(&mut scratch, event, pokemon_dex);
    }
    lines.push("endofturn".to_string());
    Ok(lines.join("\n"))
}

fn render_standalone_tree(
    event: &InformationEvent,
    belief: &UnknownBattleState,
    lines: &mut Vec<String>,
) -> Result<(), Unsupported> {
    match &event.kind {
        EventKind::DamageDealt { target, new_hp, .. } => lines.push(format!(
            "{} damage {}",
            slot_token(*target),
            raw_hp_token(new_hp)
        )),
        EventKind::Healed { target, new_hp, .. } => lines.push(format!(
            "{} heal {}",
            slot_token(*target),
            raw_hp_token(new_hp)
        )),
        EventKind::SetHp { target, new_hp, .. } => lines.push(format!(
            "{} sethp {}",
            slot_token(*target),
            raw_hp_token(new_hp)
        )),
        EventKind::Faint { .. } => {}
        EventKind::AbilityRevealed { .. }
        | EventKind::ItemRevealed { .. }
        | EventKind::ItemLost { .. }
        | EventKind::ItemGained { .. }
        | EventKind::WeatherChanged { .. }
        | EventKind::TerrainChanged { .. }
        | EventKind::PseudoWeatherStart { .. }
        | EventKind::PseudoWeatherEnd { .. }
        | EventKind::VolatileEnd { .. }
        | EventKind::StatusInflicted { .. }
        | EventKind::StatusCured { .. }
        | EventKind::BoostChanged { .. }
        | EventKind::BoostsCopied { .. }
        | EventKind::BoostsInverted { .. }
        | EventKind::SideConditionStart { .. }
        | EventKind::SideConditionEnd { .. } => lines.push(render_standalone_event(event, belief)?),
        other => {
            return Err(unsupported(format!(
                "{other:?} has no standalone end-of-turn tracker grammar yet"
            )));
        }
    }
    for reaction in &event.reactions {
        render_standalone_tree(reaction, belief, lines)?;
    }
    Ok(())
}

/// Render one top-level event to a single line, or `Ok(None)` if the event
/// (after subtracting guaranteed-and-therefore-re-synthesized reactions)
/// carries no information a user would need to type at all.
fn render_top_level_event(
    event: &InformationEvent,
    belief: &UnknownBattleState,
    move_dex: &HashMap<PokemonMove, MoveData>,
    pokemon_dex: &HashMap<Species, PokemonData>,
) -> Result<Option<String>, Unsupported> {
    let explicit_reactions =
        reactions_requiring_explicit_render(event, belief, move_dex, pokemon_dex);

    match &event.kind {
        EventKind::MoveUsed {
            user,
            move_used,
            targets,
        } => {
            let mut line = format!("{} {}", slot_token(*user), move_word(move_used));
            for &target in targets {
                let _ = write!(line, " @{}", slot_token(target));
            }
            for r in &explicit_reactions {
                let _ = write!(
                    line,
                    " {}",
                    render_move_qualifier(r, belief, move_dex, pokemon_dex)?
                );
            }
            for (_, qualifier) in
                missing_guaranteed_status_blockers(event, belief, move_dex, pokemon_dex)
            {
                let _ = write!(line, " {qualifier}");
            }
            Ok(Some(line))
        }
        EventKind::Switch(sw) => {
            let mut out = vec![switch_line(sw)?];
            let mut after_switch = belief.clone();
            fold_event_into_synthesis_scratch(
                &mut after_switch,
                &InformationEvent {
                    kind: event.kind.clone(),
                    reactions: Vec::new(),
                },
                pokemon_dex,
            );
            for reaction in &explicit_reactions {
                out.push(render_standalone_event(reaction, &after_switch)?);
                for child in reactions_requiring_explicit_render(
                    reaction,
                    &after_switch,
                    move_dex,
                    pokemon_dex,
                ) {
                    render_standalone_tree(child, &after_switch, &mut out)?;
                }
            }
            Ok(Some(out.join("\n")))
        }
        EventKind::SimultaneousSwitch { switches } => {
            // Rendered as ONE combined `leads p ... o ...` line covering
            // every side that qualifies (mirroring the grammar a user
            // types — see `tracker_parse.rs`'s `"leads"` dispatch arm).
            // `fold_leads_and_entry_abilities` also accepts two separate
            // per-side `leads` lines, but a single combined line is the
            // canonical round-trip rendering. Entry-ability reveals nested
            // as reactions are rendered as their own follow-up lines; their
            // guaranteed cascades are re-synthesized by `augment_turn`, same
            // diffing discipline as everywhere else.
            let mut out = Vec::new();
            let mut after_switch = belief.clone();
            fold_event_into_synthesis_scratch(
                &mut after_switch,
                &InformationEvent {
                    kind: event.kind.clone(),
                    reactions: Vec::new(),
                },
                pokemon_dex,
            );
            let mut leads_fragments: Vec<String> = Vec::new();
            let mut fallback_switch_lines: Vec<String> = Vec::new();
            for player in [Player::P1, Player::P2] {
                let mut side: Vec<&SwitchState> = switches
                    .iter()
                    .filter(|sw| sw.slot.player == player)
                    .collect();
                if side.is_empty() {
                    continue;
                }
                // Simulator action order is speed/queue order, not slot order;
                // `leads` assigns species left-to-right to slots 0, 1, ... .
                side.sort_by_key(|sw| sw.slot.slot_index);
                let side_was_empty = match player {
                    Player::P1 => belief.p1_active_mons.is_empty(),
                    Player::P2 => belief.p2_active_mons.is_empty(),
                };
                let fills_from_left = side
                    .iter()
                    .enumerate()
                    .all(|(index, sw)| sw.slot.slot_index as usize == index);
                if side_was_empty && fills_from_left {
                    let species_list = side
                        .iter()
                        .map(|sw| species_word(&sw.species))
                        .collect::<Vec<_>>()
                        .join(" ");
                    leads_fragments.push(format!("{} {}", side_word(player), species_list));
                } else {
                    // A simultaneous post-faint replacement may fill only a
                    // subset of doubles slots. `leads` cannot preserve those
                    // indices, so emit ordinary slot-addressed switches.
                    fallback_switch_lines.extend(
                        side.iter()
                            .map(|sw| switch_line(sw))
                            .collect::<Result<Vec<_>, _>>()?,
                    );
                }
            }
            if !leads_fragments.is_empty() {
                out.push(format!("leads {}", leads_fragments.join(" ")));
            }
            out.extend(fallback_switch_lines);
            for r in &explicit_reactions {
                out.push(render_standalone_event(r, &after_switch)?);
                for child in
                    reactions_requiring_explicit_render(r, &after_switch, move_dex, pokemon_dex)
                {
                    render_standalone_tree(child, &after_switch, &mut out)?;
                }
                // Entry abilities are ordered siblings. Thread each one's
                // actual reactions into the scratch state before diffing the
                // next (for example Drizzle followed by Sand Stream); otherwise
                // the second ability is compared against the pre-entry weather
                // and its guaranteed override is rendered a second time.
                fold_event_into_synthesis_scratch(&mut after_switch, r, pokemon_dex);
            }
            Ok(Some(out.join("\n")))
        }
        EventKind::MegaEvolution { slot, into } => {
            let mut out = vec![format!(
                "{} mega {}",
                slot_token(*slot),
                species_word(into)
            )];
            for reaction in &explicit_reactions {
                render_standalone_tree(reaction, belief, &mut out)?;
            }
            Ok(Some(out.join("\n")))
        }
        EventKind::Terastallization { slot, tera_type } => Ok(Some(format!(
            "{} tera {}",
            slot_token(*slot),
            type_word(tera_type)
        ))),
        EventKind::Cant { slot, reason } => {
            let mut out = vec![format!(
                "{} {}",
                slot_token(*slot),
                cant_reason_word(reason)?
            )];
            for reaction in &explicit_reactions {
                render_standalone_tree(reaction, belief, &mut out)?;
            }
            Ok(Some(out.join("\n")))
        }
        EventKind::MustRecharge { slot } => Ok(Some(format!("{} mustrecharge", slot_token(*slot)))),
        EventKind::AbilityRevealed { .. }
        | EventKind::ItemRevealed { .. }
        | EventKind::ItemLost { .. }
        | EventKind::ItemGained { .. }
        | EventKind::WeatherChanged { .. }
        | EventKind::TerrainChanged { .. }
        | EventKind::PseudoWeatherStart { .. }
        | EventKind::PseudoWeatherEnd { .. }
        | EventKind::SideConditionStart { .. }
        | EventKind::SideConditionEnd { .. }
        | EventKind::VolatileEnd { .. }
        | EventKind::StatusCured { .. }
        | EventKind::BoostChanged { .. }
        | EventKind::BoostsCopied { .. } => render_standalone_event(event, belief).map(Some),
        // Auto-synthesized as a sibling of any zero-HP DamageDealt/Healed/SetHp
        // (`synthesize_guaranteed_faints`) — never rendered directly.
        EventKind::Faint { .. } => Ok(None),
        other => Err(unsupported(format!(
            "{other:?} has no top-level tracker grammar yet"
        ))),
    }
}

fn switch_line(sw: &SwitchState) -> Result<String, Unsupported> {
    let mut line = format!(
        "{} switch {} {} {}",
        slot_token(sw.slot),
        species_word(&sw.species),
        hp_token(&sw.hp),
        sw.status
            .as_ref()
            .map(status_word)
            .transpose()?
            .unwrap_or("healthy")
    );
    if let Some(tera_type) = &sw.tera_type {
        let _ = write!(line, " tera-{}", type_word(tera_type));
    }
    Ok(line)
}

fn missing_guaranteed_status_blockers(
    event: &InformationEvent,
    belief: &UnknownBattleState,
    move_dex: &HashMap<PokemonMove, MoveData>,
    pokemon_dex: &HashMap<Species, PokemonData>,
) -> Vec<(FieldSlot, String)> {
    let EventKind::MoveUsed { .. } = &event.kind else {
        return Vec::new();
    };
    let bare = InformationEvent {
        kind: event.kind.clone(),
        reactions: Vec::new(),
    };
    let synthetic = augment_with_guaranteed_effects(bare, belief, move_dex, pokemon_dex);
    synthetic
        .reactions
        .iter()
        .filter_map(|expected| {
            let EventKind::StatusInflicted { target, status } = &expected.kind else {
                return None;
            };
            let observed = event.reactions.iter().any(|actual| {
                matches!(
                    &actual.kind,
                    EventKind::StatusInflicted { target: actual_target, status: actual_status }
                        if actual_target == target && actual_status == status
                )
            });
            let already_failed = event.reactions.iter().any(|actual| {
                matches!(
                    &actual.kind,
                    EventKind::Missed { target: actual_target }
                        | EventKind::Immune { target: actual_target }
                        | EventKind::Blocked { target: actual_target }
                        if actual_target == target
                ) || matches!(actual.kind, EventKind::MoveFailed { .. })
            });
            (!observed && !already_failed)
                .then(|| (*target, format!("{} blocked", slot_token(*target))))
        })
        .collect()
}

/// Render a standalone `[slot] [word]` line for an event that also makes
/// sense as its own line (ability/item reveal nested under a switch, etc.).
fn render_standalone_event(
    event: &InformationEvent,
    _belief: &UnknownBattleState,
) -> Result<String, Unsupported> {
    match &event.kind {
        EventKind::AbilityRevealed { slot, ability } => {
            Ok(format!("{} {}", slot_token(*slot), ability_word(ability)))
        }
        EventKind::ItemRevealed { slot, item } => {
            Ok(format!("{} {}", slot_token(*slot), item_word(item)))
        }
        EventKind::ItemLost {
            slot,
            item,
            consumed,
        } => Ok(format!(
            "{} {} {}",
            slot_token(*slot),
            if *consumed { "consumes" } else { "loses" },
            item_word(item)
        )),
        EventKind::ItemGained { slot, item } => {
            Ok(format!("{} gains {}", slot_token(*slot), item_word(item)))
        }
        EventKind::WeatherChanged { weather: Some(w) } => {
            Ok(format!("weather {}", weather_word(w)))
        }
        EventKind::WeatherChanged { weather: None } => Ok("weather none".to_string()),
        EventKind::TerrainChanged { terrain: Some(t) } => {
            Ok(format!("terrain {}", terrain_word(t)))
        }
        EventKind::TerrainChanged { terrain: None } => Ok("terrain none".to_string()),
        EventKind::PseudoWeatherStart { effect } => {
            Ok(format!("field {} start", pseudo_weather_word(effect)))
        }
        EventKind::PseudoWeatherEnd { effect } => {
            Ok(format!("field {} end", pseudo_weather_word(effect)))
        }
        EventKind::VolatileEnd { target, volatile } => Ok(format!(
            "{} volatileend {}",
            slot_token(*target),
            volatile_word(volatile)?
        )),
        EventKind::StatusCured { target, status } => Ok(format!(
            "{} cure {}",
            slot_token(*target),
            status_word(status)?
        )),
        EventKind::StatusInflicted { target, status } => Ok(format!(
            "{} status {}",
            slot_token(*target),
            status_word(status)?
        )),
        EventKind::BoostChanged {
            target,
            boost_idx,
            stages,
        } => Ok(format!(
            "{} {}",
            slot_token(*target),
            boost_token(*boost_idx, *stages)?
        )),
        EventKind::BoostsCopied { source, target } => Ok(format!(
            "{} copyboosts {}",
            slot_token(*target),
            slot_token(*source)
        )),
        EventKind::BoostsInverted { target } => Ok(format!("{} invertboosts", slot_token(*target))),
        EventKind::SideConditionStart { side, condition } => Ok(format!(
            "side {} {} start",
            side_word(*side),
            side_condition_word(condition)
        )),
        EventKind::SideConditionEnd { side, condition } => Ok(format!(
            "side {} {} end",
            side_word(*side),
            side_condition_word(condition)
        )),
        other => Err(unsupported(format!(
            "{other:?} has no standalone tracker grammar yet"
        ))),
    }
}

/// Render one reaction as an inline token appended to a move line (the
/// "flat nesting" convention `tracker_parse.rs` itself uses).
fn render_move_qualifier(
    event: &InformationEvent,
    belief: &UnknownBattleState,
    move_dex: &HashMap<PokemonMove, MoveData>,
    pokemon_dex: &HashMap<Species, PokemonData>,
) -> Result<String, Unsupported> {
    let mut rendered = match &event.kind {
        EventKind::Crit { target } => Ok(format!("{} crit", slot_token(*target))),
        EventKind::Missed { target } => Ok(format!("{} miss", slot_token(*target))),
        EventKind::Immune { target } => Ok(format!("{} immune", slot_token(*target))),
        EventKind::Blocked { target } => Ok(format!("{} blocked", slot_token(*target))),
        EventKind::MoveFailed { slot } => Ok(format!("{} fail", slot_token(*slot))),
        EventKind::MustRecharge { slot } => Ok(format!("{} mustrecharge", slot_token(*slot))),
        // Bare `charging`, no move name: it is always this line's own move (the
        // parser rejects any other), so repeating it is noise. `move_used` is
        // matched-and-ignored rather than `..`-ed so that a future variant change
        // surfaces here as a compile error.
        EventKind::ChargingMove {
            user,
            move_used: _,
        } => Ok(format!("{} charging", slot_token(*user))),
        EventKind::IllusionEnded {
            slot,
            actual_species,
        } => Ok(format!(
            "{} illusion {}",
            slot_token(*slot),
            species_word(actual_species)
        )),
        EventKind::Switch(switch) => switch_line(switch),
        EventKind::HitCount { target, hits } => Ok(format!("{} {hits}hits", slot_token(*target))),
        EventKind::DamageDealt { target, new_hp, .. } => Ok(format!(
            "{} damage {}",
            slot_token(*target),
            raw_hp_token(new_hp)
        )),
        EventKind::Healed { target, new_hp, .. } => Ok(format!(
            "{} heal {}",
            slot_token(*target),
            raw_hp_token(new_hp)
        )),
        EventKind::SetHp { target, new_hp, .. } => Ok(format!(
            "{} sethp {}",
            slot_token(*target),
            raw_hp_token(new_hp)
        )),
        EventKind::StatusInflicted { target, status } => {
            Ok(format!("{} {}", slot_token(*target), status_word(status)?))
        }
        EventKind::StatusCured { target, status } => Ok(format!(
            "{} cure {}",
            slot_token(*target),
            status_word(status)?
        )),
        EventKind::VolatileStart {
            target,
            volatile: VolatileStatus::Encore(move_used),
        } => Ok(format!(
            "{} encoremove {}",
            slot_token(*target),
            move_word(move_used)
        )),
        EventKind::VolatileStart {
            target,
            volatile: VolatileStatus::Disable(move_used),
        } => Ok(format!(
            "{} disablemove {}",
            slot_token(*target),
            move_word(move_used)
        )),
        EventKind::VolatileStart {
            target,
            volatile: VolatileStatus::Stockpile(level),
        } => Ok(format!("{} stockpilelevel {level}", slot_token(*target))),
        EventKind::VolatileStart { target, volatile } => Ok(format!(
            "{} {}",
            slot_token(*target),
            volatile_word(volatile)?
        )),
        EventKind::VolatileEnd { target, volatile } => Ok(format!(
            "{} volatileend {}",
            slot_token(*target),
            volatile_word(volatile)?
        )),
        EventKind::BoostChanged {
            target,
            boost_idx,
            stages,
        } => Ok(format!(
            "{} {}",
            slot_token(*target),
            boost_token(*boost_idx, *stages)?
        )),
        EventKind::BoostsCopied { source, target } => Ok(format!(
            "{} copyboosts {}",
            slot_token(*target),
            slot_token(*source)
        )),
        EventKind::BoostsInverted { target } => Ok(format!("{} invertboosts", slot_token(*target))),
        EventKind::ItemLost {
            slot,
            item,
            consumed,
        } => Ok(format!(
            "{} {} {}",
            slot_token(*slot),
            if *consumed { "consumes" } else { "loses" },
            item_word(item)
        )),
        EventKind::ItemGained { slot, item } => {
            Ok(format!("{} gains {}", slot_token(*slot), item_word(item)))
        }
        EventKind::ItemRevealed { slot, item } => {
            Ok(format!("{} {}", slot_token(*slot), item_word(item)))
        }
        EventKind::AbilityRevealed { slot, ability } => {
            Ok(format!("{} {}", slot_token(*slot), ability_word(ability)))
        }
        EventKind::PseudoWeatherStart { effect } => {
            Ok(format!("field {} start", pseudo_weather_word(effect)))
        }
        EventKind::PseudoWeatherEnd { effect } => {
            Ok(format!("field {} end", pseudo_weather_word(effect)))
        }
        EventKind::SideConditionStart { side, condition } => Ok(format!(
            "side {} {} start",
            side_word(*side),
            side_condition_word(condition)
        )),
        EventKind::SideConditionEnd { side, condition } => Ok(format!(
            "side {} {} end",
            side_word(*side),
            side_condition_word(condition)
        )),
        // Field-level payloads of a move's own secondaries — rendered as a
        // standalone-style word appended inline; the target-agnostic ones
        // (weather/terrain) don't need a slot prefix on a move line since
        // they're global/side-scoped, not per-mon.
        EventKind::WeatherChanged { weather: Some(w) } => {
            Ok(format!("weather {}", weather_word(w)))
        }
        EventKind::TerrainChanged { terrain: Some(t) } => {
            Ok(format!("terrain {}", terrain_word(t)))
        }
        other => {
            return Err(unsupported(format!(
                "{other:?} has no inline move-qualifier tracker grammar yet"
            )));
        }
    }?;

    // The simulator often nests observations below their trigger (Crit below
    // DamageDealt, recoil below an AbilityRevealed, berry healing below
    // ItemLost). Tracker syntax is intentionally flat, so recursively append
    // only children that the production augmenter will not recreate.
    let mut reaction_scratch;
    let reaction_belief = if matches!(event.kind, EventKind::Switch(_)) {
        reaction_scratch = belief.clone();
        fold_event_into_synthesis_scratch(
            &mut reaction_scratch,
            &InformationEvent {
                kind: event.kind.clone(),
                reactions: Vec::new(),
            },
            pokemon_dex,
        );
        &reaction_scratch
    } else {
        belief
    };
    for reaction in
        reactions_requiring_explicit_render(event, reaction_belief, move_dex, pokemon_dex)
    {
        let _ = write!(
            rendered,
            " {}",
            render_move_qualifier(reaction, reaction_belief, move_dex, pokemon_dex)?
        );
    }
    Ok(rendered)
}

/// Reactions of `event` that `augment_with_guaranteed_effects` would NOT
/// already re-synthesize on its own for a bare (reaction-less) clone — see
/// this module's doc comment for why this diff is necessary. Only compares
/// one level deep (direct children), matching the shapes
/// `augment_with_guaranteed_effects` itself produces for `MoveUsed`/
/// `MegaEvolution`/`AbilityRevealed`. When an otherwise guaranteed node has
/// additional simulator-only descendants, recursively subtract the
/// synthesized subtree so those observations are still rendered.
fn reactions_requiring_explicit_render<'a>(
    event: &'a InformationEvent,
    belief: &UnknownBattleState,
    move_dex: &HashMap<PokemonMove, MoveData>,
    pokemon_dex: &HashMap<Species, PokemonData>,
) -> Vec<&'a InformationEvent> {
    let bare = InformationEvent {
        kind: event.kind.clone(),
        reactions: Vec::new(),
    };
    let synthetic = augment_with_guaranteed_effects(bare, belief, move_dex, pokemon_dex);
    let mut remaining_synthetic = synthetic.reactions;
    let mut explicit = Vec::new();
    for r in &event.reactions {
        if let EventKind::Faint { slot } = &r.kind
            && event.reactions.iter().any(|sibling| match &sibling.kind {
                EventKind::DamageDealt { target, new_hp, .. }
                | EventKind::Healed { target, new_hp, .. }
                | EventKind::SetHp { target, new_hp, .. } => {
                    target == slot && matches!(new_hp, PokemonHP::Number(0) | PokemonHP::Percent(0))
                }
                _ => false,
            })
        {
            explicit.extend(r.reactions.iter());
            continue;
        }
        if matches!(r.kind, EventKind::Faint { .. })
            && matches!(
                &event.kind,
                EventKind::DamageDealt {
                    new_hp: PokemonHP::Number(0) | PokemonHP::Percent(0),
                    ..
                } | EventKind::Healed {
                    new_hp: PokemonHP::Number(0) | PokemonHP::Percent(0),
                    ..
                } | EventKind::SetHp {
                    new_hp: PokemonHP::Number(0) | PokemonHP::Percent(0),
                    ..
                }
            )
        {
            explicit.extend(r.reactions.iter());
            continue;
        }
        // Protect-style resolution emits this bookkeeping child even
        // though the enclosing MoveUsed already records the same user and
        // move. The tracker parser intentionally represents the action
        // once; inference treats MoveUsed as the same committed action.
        if let (
            EventKind::MoveUsed {
                user, move_used, ..
            },
            EventKind::SingleMoveOrTurn {
                slot,
                move_used: child_move,
            },
        ) = (&event.kind, &r.kind)
            && user == slot
            && move_used == child_move
        {
            continue;
        }
        if let Some(pos) = remaining_synthetic.iter().position(|s| s == r) {
            remaining_synthetic.remove(pos);
            continue;
        }
        // A guaranteed node may carry additional simulator-only children
        // (for example an Intimidate drop containing a Defiant reveal and
        // its +2 Atk reaction). Match the guaranteed parent by kind, suppress
        // that parent token, and recursively preserve only the descendants
        // that synthesis would not recreate.
        if let Some(pos) = remaining_synthetic.iter().position(|s| s.kind == r.kind) {
            let synthetic_match = remaining_synthetic.remove(pos);
            collect_non_synthetic_reactions(r, &synthetic_match, &mut explicit);
            continue;
        }
        explicit.push(r);
    }
    explicit
}

fn collect_non_synthetic_reactions<'a>(
    actual: &'a InformationEvent,
    synthetic: &InformationEvent,
    explicit: &mut Vec<&'a InformationEvent>,
) {
    let mut remaining_synthetic = synthetic.reactions.clone();
    for reaction in &actual.reactions {
        if let Some(pos) = remaining_synthetic
            .iter()
            .position(|candidate| candidate == reaction)
        {
            remaining_synthetic.remove(pos);
            continue;
        }
        if let Some(pos) = remaining_synthetic
            .iter()
            .position(|candidate| candidate.kind == reaction.kind)
        {
            let synthetic_match = remaining_synthetic.remove(pos);
            collect_non_synthetic_reactions(reaction, &synthetic_match, explicit);
            continue;
        }
        explicit.push(reaction);
    }
}

// ── Word rendering — inverse of tracker_parse.rs's word tables ─────────────

fn slot_token(slot: FieldSlot) -> String {
    format!(
        "{}{}",
        match slot.player {
            Player::P1 => "p",
            Player::P2 => "o",
        },
        slot.slot_index + 1
    )
}

fn side_word(player: Player) -> &'static str {
    match player {
        Player::P1 => "p",
        Player::P2 => "o",
    }
}

/// `Species`/`PokemonMove`/`Item`/`Ability`'s `from_str` are generated
/// directly from their normalized Debug/display names (see `norm`'s doc
/// comment in `tracker_parse.rs`), so lowercasing+stripping punctuation from
/// `{:?}` round-trips through them exactly.
fn norm_debug<T: std::fmt::Debug>(v: &T) -> String {
    format!("{v:?}")
        .chars()
        .filter(|c| c.is_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

fn species_word(s: &Species) -> String {
    norm_debug(s)
}

fn move_word(m: &PokemonMove) -> String {
    norm_debug(m)
}

fn item_word(i: &Item) -> String {
    norm_debug(i)
}

fn ability_word(a: &Ability) -> String {
    norm_debug(a)
}

fn type_word(t: &PokemonType) -> String {
    format!("{t:?}")
}

fn hp_token(hp: &PokemonHP) -> String {
    match hp {
        PokemonHP::Number(n) => format!("{n}hp"),
        PokemonHP::Percent(p) => format!("{p}%"),
    }
}

fn raw_hp_token(hp: &PokemonHP) -> String {
    hp_token(hp)
}

fn status_word(status: &Status) -> Result<&'static str, Unsupported> {
    Ok(match status {
        Status::Burn => "brn",
        Status::Poison => "psn",
        Status::ToxicPoison(_) => "tox",
        Status::Paralysis => "par",
        Status::Sleep(_) => "slp",
        Status::Frozen(_) => "frz",
    })
}

fn volatile_word(v: &VolatileStatus) -> Result<&'static str, Unsupported> {
    use VolatileStatus::*;
    Ok(match v {
        Confusion => "confusion",
        LeechSeed => "leechseed",
        Taunt => "taunt",
        FlashFire => "flashfire",
        FocusEnergy => "focusenergy",
        AquaRing => "aquaring",
        Attract => "attract",
        Curse => "curse",
        Torment => "torment",
        Yawn => "yawn",
        SaltCure => "saltcure",
        TarShot => "tarshot",
        Minimize => "minimize",
        Ingrain => "ingrain",
        MagnetRise => "magnetrise",
        Protect => "protect",
        Endure => "endure",
        KingsShield => "kingsshield",
        BanefulBunker => "banefulbunker",
        SpikyShield => "spikyshield",
        SilkTrap => "silktrap",
        Obstruct => "obstruct",
        BurningBulwark => "burningbulwark",
        DestinyBond => "destinybond",
        Grudge => "grudge",
        Embargo => "embargo",
        HealBlock => "healblock",
        Imprison => "imprison",
        Electrify => "electrify",
        Powder => "powder",
        SyrupBomb => "syrupbomb",
        Telekinesis => "telekinesis",
        SmackDown => "smackdown",
        Uproar => "uproar",
        Roost => "roost",
        Rage => "rage",
        RagePowder => "ragepowder",
        FollowMe => "followme",
        MagicCoat => "magiccoat",
        Snatch => "snatch",
        LaserFocus => "laserfocus",
        MiracleEye => "miracleeye",
        Foresight => "foresight",
        OctoLock => "octolock",
        NoRetreat => "noretreat",
        GastroAcid => "gastroacid",
        SparklingAria => "sparklingaria",
        GlaiveRush => "glaiverush",
        Charge => "charge",
        DefenseCurl => "defensecurl",
        HelpingHand => "helpinghand",
        PowerTrick => "powertrick",
        ForestsCurse => "forestscurse",
        ThroatChop => "throatchop",
        MustRecharge => "mustrecharge",
        Substitute(_) => "substitute",
        Encore(_) => "encore",
        Disable(_) => "disable",
        other => {
            return Err(unsupported(format!(
                "{other:?} has no volatile tracker word yet"
            )));
        }
    })
}

fn boost_token(idx: usize, stages: i8) -> Result<String, Unsupported> {
    let name = match idx {
        0 => "atk",
        1 => "def",
        2 => "spa",
        3 => "spd",
        4 => "spe",
        5 => "acc",
        6 => "eva",
        other => return Err(unsupported(format!("boost_idx {other} out of range"))),
    };
    Ok(format!("{name}{stages:+}"))
}

fn weather_word(w: &Weather) -> &'static str {
    match w {
        Weather::Rain => "rain",
        Weather::HeavyRain => "heavyrain",
        Weather::Sandstorm => "sand",
        Weather::Snow => "snow",
        Weather::Sun => "sun",
        Weather::ExtremeSunlight => "extremesun",
        Weather::StrongWinds => "strongwinds",
    }
}

fn terrain_word(t: &Terrain) -> &'static str {
    match t {
        Terrain::ElectricTerrain => "electric",
        Terrain::GrassyTerrain => "grassy",
        Terrain::MistyTerrain => "misty",
        Terrain::PsychicTerrain => "psychic",
    }
}

fn pseudo_weather_word(effect: &poke_rust::state::dex_data::PseudoWeather) -> &'static str {
    use poke_rust::state::dex_data::PseudoWeather::*;
    match effect {
        FairyLock => "fairylock",
        Gravity => "gravity",
        IonDeluge => "iondeluge",
        MagicDeluge => "magicdeluge",
        MudSport => "mudsport",
        TrickRoom => "trickroom",
        WaterSport => "watersport",
        WonderRoom => "wonderroom",
    }
}

fn side_condition_word(condition: &poke_rust::state::dex_data::SideCondition) -> &'static str {
    use poke_rust::state::dex_data::SideCondition::*;
    match condition {
        AuroraVeil => "auroraveil",
        Reflect => "reflect",
        CraftyShield => "craftyshield",
        LightScreen => "lightscreen",
        LuckyChant => "luckychant",
        MatBlock => "matblock",
        Mist => "mist",
        QuickGuard => "quickguard",
        SafeGuard => "safeguard",
        Spikes(0) => "spikes0",
        Spikes(1) => "spikes",
        Spikes(2) => "spikes2",
        Spikes(_) => "spikes3",
        StealthRock => "stealthrock",
        StickyWeb(_) => "stickyweb",
        TailWind => "tailwind",
        ToxicSpikes(0) => "toxicspikes0",
        ToxicSpikes(1) => "toxicspikes",
        ToxicSpikes(_) => "toxicspikes2",
        WideGuard => "wideguard",
    }
}

fn cant_reason_word(
    reason: &poke_rust::information::information::CantReason,
) -> Result<&'static str, Unsupported> {
    use poke_rust::information::information::CantReason::*;
    Ok(match reason {
        Flinch => "flinch",
        Paralysis => "fullpara",
        Sleep => "sleep",
        Freeze => "frozen",
        Recharge => "recharge",
        Taunt => "taunt",
        Disable => "disable",
        Confusion => "confusion",
        Imprison => "imprison",
        Infatuation => "attract",
        Bound => "bound",
        ThroatChop => "throatchop",
        Torment => "torment",
        FocusPunch => "focuspunch",
        Gravity => "gravity",
        HealBlock => "healblock",
        Encore => "encore",
        other => {
            return Err(unsupported(format!(
                "{other:?} has no cant-reason tracker word yet"
            )));
        }
    })
}

#[cfg(test)]
mod tests {
    //! Round-trip tracker-fidelity test: a REAL simulated battle's own
    //! events, rendered to tracker text and fed through the actual
    //! parse -> fold -> augment -> apply_information pipeline
    //! (`submit_tracker_events`'s own sequence — see `tracker.rs`), must
    //! produce a belief that is a sound superset of the true resulting state
    //! — reusing the SAME subset oracle the engine's own inference fuzz
    //! tests use (`information::subset_check`), per the plan this
    //! implements: "if I have a simulated run and manually input the events
    //! that are happening into the tracker then it should be accurately
    //! tracking the state of the game."
    //!
    //! This regression is deliberately a small, hand-authored scenario; the
    //! randomized sweep later in this module provides broad battle coverage.
    //! What is exercised here is
    //! the full, real pipeline (render_turn's diffing-against-synthesis
    //! logic, `fold_leads_and_entry_abilities`, `augment_turn`, and
    //! `apply_information`) against genuine simulated data, covering: a lead
    //! entry-ability weather trigger (Sand Stream), a guaranteed-secondary
    //! status move (Thunder Wave), and an ordinary damaging move.
    use super::*;
    use crate::session::Dexes;
    use crate::tracker::{TrackerSession, apply_tracker_text};
    use poke_rust::information::inference::{InferenceConfig, apply_information};
    use poke_rust::information::information::mask_events_for;
    use poke_rust::information::subset_check::assert_true_state_subset_of_belief;
    use poke_rust::information::unknowns::UnknownMatchState;
    use poke_rust::state::battle::{
        AttackCommand, BattleCommand, BattleState, MatchState, PlayerCommand, TeamPreviewCommand,
    };
    use poke_rust::state::dex_data::{
        AbilityData, parse_ability_dex, parse_move_dex, parse_pokemon_dex,
    };
    use rand::rngs::StdRng;
    use rand::{Rng, SeedableRng};
    use std::sync::OnceLock;

    static POKEMON_DEX: OnceLock<HashMap<Species, PokemonData>> = OnceLock::new();
    static MOVE_DEX: OnceLock<HashMap<PokemonMove, MoveData>> = OnceLock::new();
    static ABILITY_DEX: OnceLock<HashMap<Ability, AbilityData>> = OnceLock::new();

    fn pokemon_dex() -> &'static HashMap<Species, PokemonData> {
        POKEMON_DEX.get_or_init(|| parse_pokemon_dex("../pokemon_info/showdownDex.txt"))
    }
    fn move_dex() -> &'static HashMap<PokemonMove, MoveData> {
        MOVE_DEX.get_or_init(|| parse_move_dex("../pokemon_info/showdownMoves.txt"))
    }
    fn ability_dex() -> &'static HashMap<Ability, AbilityData> {
        ABILITY_DEX.get_or_init(|| parse_ability_dex("../pokemon_info/showdownAbilities.txt"))
    }

    static LEARNSET_DEX: OnceLock<HashMap<Species, std::collections::HashSet<PokemonMove>>> =
        OnceLock::new();
    fn learnset_dex() -> &'static HashMap<Species, std::collections::HashSet<PokemonMove>> {
        LEARNSET_DEX.get_or_init(|| {
            poke_rust::state::dex_data::parse_learnset_dex("../pokemon_info/showdownLearnsets.txt")
        })
    }

    static DEXES: OnceLock<Dexes> = OnceLock::new();
    fn dexes() -> &'static Dexes {
        DEXES.get_or_init(|| Dexes {
            pokemon_dex: parse_pokemon_dex("../pokemon_info/showdownDex.txt"),
            move_dex: parse_move_dex("../pokemon_info/showdownMoves.txt"),
            ability_dex: parse_ability_dex("../pokemon_info/showdownAbilities.txt"),
            learnset_dex: learnset_dex().clone(),
        })
    }

    const TEAM_P1: &str = "Pikachu @ Light Ball\nAbility: Static\nLevel: 50\nEVs: 4 HP / 252 SpA / 252 Spe\nModest Nature\n- Thunderbolt\n- Thunder Wave\n";
    const TEAM_P2: &str = "Tyranitar @ Sitrus Berry\nAbility: Sand Stream\nLevel: 50\nEVs: 252 HP / 4 Atk / 252 SpD\nCareful Nature\n- Tackle\n- Crunch\n";

    // A deliberately renderer-complete corpus for the gating fuzz sweep. It
    // still exercises doubles targeting, switching, damage, misses, crits,
    // status, Protect, Tera, fainting, and replacements without relying on
    // event kinds for which tracker text has no grammar yet.
    const FUZZ_TEAM_P1: &str = "Pikachu\nAbility: Static\nLevel: 50\n- Thunderbolt\n- Thunder Wave\n- Quick Attack\n- Protect\n\nCharizard\nAbility: Blaze\nLevel: 50\n- Flamethrower\n- Air Slash\n- Dragon Claw\n- Protect\n\nGyarados\nAbility: Intimidate\nLevel: 50\n- Waterfall\n- Thunder Wave\n- Crunch\n- Protect\n\nLucario\nAbility: Inner Focus\nLevel: 50\n- Aura Sphere\n- Flash Cannon\n- Extreme Speed\n- Protect\n";
    const FUZZ_TEAM_P2: &str = "Garchomp\nAbility: Rough Skin\nLevel: 50\n- Dragon Claw\n- Earthquake\n- Rock Slide\n- Protect\n\nTyranitar\nAbility: Unnerve\nLevel: 50\n- Rock Slide\n- Crunch\n- Earthquake\n- Protect\n\nSylveon\nAbility: Pixilate\nLevel: 50\n- Moonblast\n- Hyper Voice\n- Quick Attack\n- Protect\n\nAerodactyl\nAbility: Unnerve\nLevel: 50\n- Rock Slide\n- Aerial Ace\n- Crunch\n- Protect\n";

    const REAL_FUZZ_TEAMSHEETS: [&str; 14] = [
        "../teamsheets/MA_charizard_sylveon.txt",
        "../teamsheets/MA_dragonite_rain.txt",
        "../teamsheets/MA_floette_froslass.txt",
        "../teamsheets/MA_tyranitar_zoroark.txt",
        "../teamsheets/MA_venusaur_aerodactl.txt",
        "../teamsheets/MB_aboma_pidgeon.txt",
        "../teamsheets/MB_barbaracle_zoroark.txt",
        "../teamsheets/MB_espathra_scovillain.txt",
        "../teamsheets/MB_gallade_clefable.txt",
        "../teamsheets/MB_gyarados_volcarona.txt",
        "../teamsheets/MB_malamar_tr.txt",
        "../teamsheets/MB_raptor_stuff.txt",
        "../teamsheets/MB_sand_doggo_rat.txt",
        "../teamsheets/MB_vivillon_camerupt.txt",
    ];

    const TRACKER_FUZZ_SAMPLE_SALT: u64 = 0x5452_4143_4b45_5253;

    fn fuzz_env_u64(name: &str, default: u64) -> u64 {
        std::env::var(name)
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(default)
    }

    fn fuzz_env_bool(name: &str) -> bool {
        std::env::var(name)
            .ok()
            .is_some_and(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
    }

    fn random_team_preview_command(team_len: usize, rng: &mut StdRng) -> TeamPreviewCommand {
        use rand::seq::SliceRandom;
        let mut indices: Vec<usize> = (0..team_len).collect();
        indices.shuffle(rng);
        indices.truncate(4.min(team_len));
        TeamPreviewCommand {
            active_indices: indices[..2.min(indices.len())].to_vec(),
            back_indices: indices[2.min(indices.len())..].to_vec(),
        }
    }

    fn random_commands_for_player(
        state: &BattleState,
        player: Player,
        rng: &mut StdRng,
    ) -> Vec<BattleCommand> {
        let active_len = match player {
            Player::P1 => state.p1_active_mons.len(),
            Player::P2 => state.p2_active_mons.len(),
        };
        let options: Vec<Vec<BattleCommand>> = (0..active_len)
            .map(|slot| {
                poke_rust::simulator::get_possible_commands_for_active_slot(
                    state,
                    player,
                    slot,
                    move_dex(),
                    pokemon_dex(),
                )
            })
            .collect();

        for _ in 0..32 {
            let commands: Vec<BattleCommand> = options
                .iter()
                .map(|slot_options| slot_options[rng.gen_range(0..slot_options.len())].clone())
                .collect();
            if poke_rust::simulator::validate_battle_command_combination(&commands) {
                return commands;
            }
        }
        options
            .iter()
            .map(|slot_options| {
                slot_options
                    .iter()
                    .find(|command| !matches!(command, BattleCommand::Switch(_)))
                    .or_else(|| slot_options.first())
                    .cloned()
                    .unwrap_or(BattleCommand::Pass)
            })
            .collect()
    }

    fn fully_benched_tracker_session(
        preview: &poke_rust::state::battle::TeamPreviewState,
    ) -> TrackerSession {
        let UnknownMatchState::TeamPreview(team_preview_belief) =
            UnknownMatchState::team_preview_closed_sheet_from_perspective(
                Player::P1,
                &preview.p1_mons,
                &preview.p2_mons,
                pokemon_dex(),
                2,
                4,
                50,
                true,
            )
        else {
            unreachable!()
        };
        let all_p1: Vec<usize> = (0..preview.p1_mons.len()).collect();
        let mut belief =
            team_preview_belief.into_battle_state(Player::P1, &[], &all_p1, &[], &[]);
        belief.back_mons_per_side = 2;
        let mut roster_species: Vec<Species> = Vec::new();
        for mon in preview.p1_mons.iter().chain(preview.p2_mons.iter()) {
            if !roster_species.contains(&mon.species) {
                roster_species.push(mon.species.clone());
            }
        }
        TrackerSession {
            initial_belief: belief.clone(),
            belief,
            script: Vec::new(),
            active_per_side: 2,
            brought_per_side: 4,
            inference_config: InferenceConfig {
                use_stat_points: true,
                force_max_ivs: true,
                learnset_dex: learnset_dex().clone(),
                ..Default::default()
            },
            log: Vec::new(),
            turn_count: 0,
            roster_species,
        }
    }

    fn append_explicit_passes(
        mut text: String,
        command_phases: &[(PlayerCommand, PlayerCommand)],
        events: &[InformationEvent],
        game_over: bool,
    ) -> String {
        let mut passes = Vec::new();
        for (p1_cmd, p2_cmd) in command_phases {
            for (player, command) in [(Player::P1, p1_cmd), (Player::P2, p2_cmd)] {
                if let PlayerCommand::Battle(commands) = command {
                    for (slot_index, command) in commands.iter().enumerate() {
                        let slot = FieldSlot {
                            player,
                            slot_index: slot_index as u8,
                        };
                        let acted = events.iter().any(|event| match &event.kind {
                            EventKind::MoveUsed { user, .. } => *user == slot,
                            EventKind::Switch(sw) => sw.slot == slot,
                            EventKind::SimultaneousSwitch { switches } => {
                                switches.iter().any(|sw| sw.slot == slot)
                            }
                            EventKind::Cant {
                                slot: event_slot, ..
                            }
                            | EventKind::MustRecharge { slot: event_slot } => *event_slot == slot,
                            _ => false,
                        });
                        if (matches!(command, BattleCommand::Pass) || (game_over && !acted))
                            && !passes.contains(&slot)
                        {
                            passes.push(slot);
                        }
                    }
                }
            }
        }
        if passes.is_empty() {
            return text;
        }
        let sentinel = "endofturn";
        debug_assert!(text.ends_with(sentinel));
        text.truncate(text.len() - sentinel.len());
        text.push_str(
            &passes
                .into_iter()
                .map(|slot| format!("{} pass", slot_token(slot)))
                .collect::<Vec<_>>()
                .join("\n"),
        );
        text.push('\n');
        text.push_str(sentinel);
        text
    }

    /// Parse rendered text through the same structural stages the production
    /// submission path runs before inference. Keeping this separate from
    /// `apply_tracker_text` gives the fuzzer an event-level round-trip oracle:
    /// a successful belief update alone can miss silently dropped or invented
    /// observations.
    fn decode_rendered_turn(text: &str, belief: &UnknownBattleState) -> Vec<InformationEvent> {
        let lines = crate::tracker_parse::parse_tracker_text(
            text,
            belief,
            move_dex(),
            pokemon_dex(),
        )
        .unwrap_or_else(|error| {
            panic!(
                "re-parsing rendered text failed at line {}: {}\n--- tracker text ---\n{text}",
                error.line, error.message
            )
        });
        let mut events = Vec::new();
        let mut end_of_turn_reactions = Vec::new();
        for line in lines {
            match line {
                crate::tracker_parse::TrackerLine::Event(event) => events.push(event),
                crate::tracker_parse::TrackerLine::EndOfTurnReaction(event) => {
                    end_of_turn_reactions.push(event)
                }
                crate::tracker_parse::TrackerLine::EndOfTurn => events.push(InformationEvent {
                    kind: EventKind::EndOfTurn,
                    reactions: std::mem::take(&mut end_of_turn_reactions),
                }),
            }
        }
        let events = crate::tracker_parse::fold_leads_and_entry_abilities(events);
        crate::tracker_effects::augment_turn(events, belief, move_dex(), pokemon_dex())
    }

    /// Flatten the deliberately-flat tracker representation into a sorted
    /// multiset of observable event payloads. Parent/child shape and sibling
    /// order are intentionally ignored because tracker syntax attaches all
    /// move qualifiers directly to `MoveUsed`. Hidden `max_hp` is also ignored:
    /// masking keeps it as an internal companion value, while grammar exposes
    /// only the resulting exact/percent HP token. Multiplicity remains part of
    /// the comparison, so double-applied or silently dropped effects fail.
    fn canonical_event_multiset(events: &[InformationEvent]) -> Vec<String> {
        fn status_signature(status: &Status) -> &'static str {
            match status {
                Status::Burn => "Burn",
                Status::Poison => "Poison",
                Status::ToxicPoison(_) => "ToxicPoison",
                Status::Paralysis => "Paralysis",
                Status::Sleep(_) => "Sleep",
                Status::Frozen(_) => "Frozen",
            }
        }

        fn switch_signature(sw: &SwitchState) -> String {
            let status = sw.status.as_ref().map(status_signature);
            format!(
                "Switch(slot={:?},species={:?},level={},hp={:?},status={:?},tera_type={:?})",
                sw.slot, sw.species, sw.level, sw.hp, status, sw.tera_type
            )
        }

        fn visit(event: &InformationEvent, out: &mut Vec<String>) {
            match &event.kind {
                // Turn sentinels and protect-family bookkeeping duplicate the
                // enclosing action. The simulator represents a successful
                // stalling move as `SingleMoveOrTurn`; tracker augmentation
                // derives the equivalent one-turn volatile from MoveData.
                EventKind::SingleMoveOrTurn { .. } | EventKind::EndOfTurn => {}
                EventKind::VolatileStart { volatile, .. }
                    if matches!(
                        volatile,
                        VolatileStatus::Protect
                            | VolatileStatus::Endure
                            | VolatileStatus::KingsShield
                            | VolatileStatus::BanefulBunker
                            | VolatileStatus::SpikyShield
                            | VolatileStatus::SilkTrap
                            | VolatileStatus::Obstruct
                            | VolatileStatus::BurningBulwark
                            | VolatileStatus::HelpingHand
                            | VolatileStatus::Flinch
                    ) => {}
                EventKind::Switch(sw) => out.push(switch_signature(sw)),
                EventKind::SimultaneousSwitch { switches } => {
                    out.extend(switches.iter().map(switch_signature));
                }
                EventKind::DamageDealt { target, new_hp, .. } => {
                    out.push(format!("DamageDealt(target={target:?},new_hp={new_hp:?})"));
                }
                EventKind::Healed { target, new_hp, .. } => {
                    out.push(format!("Healed(target={target:?},new_hp={new_hp:?})"));
                }
                EventKind::SetHp { target, new_hp, .. } => {
                    out.push(format!("SetHp(target={target:?},new_hp={new_hp:?})"));
                }
                EventKind::StatusInflicted { target, status } => out.push(format!(
                    "StatusInflicted(target={target:?},status={})",
                    status_signature(status)
                )),
                EventKind::StatusCured { target, status } => out.push(format!(
                    "StatusCured(target={target:?},status={})",
                    status_signature(status)
                )),
                other => out.push(format!("{other:?}")),
            }
            for reaction in &event.reactions {
                visit(reaction, out);
            }
        }

        let mut out = Vec::new();
        for event in events {
            visit(event, &mut out);
        }
        out.sort_unstable();
        out
    }

    fn assert_event_round_trip(
        original: &[InformationEvent],
        decoded: &[InformationEvent],
        belief: &UnknownBattleState,
        context: &str,
        text: &str,
    ) {
        let expected = canonical_event_multiset(original);
        let mut actual = canonical_event_multiset(decoded);
        // `blocked` is currently the grammar's control word for “a guaranteed
        // status payload was inapplicable” (for example Thunder Wave into an
        // already-burned target). It deliberately suppresses augmentation but
        // is not a simulator observation in that case, so remove precisely
        // those renderer-introduced markers from the semantic comparison.
        //
        // Must thread a turn-scoped `scratch` belief through `original` in
        // order, exactly like `render_turn` does (see its doc comment) —
        // `belief` alone is the PRE-turn belief, so a target that switched in
        // earlier the SAME turn (not yet live in `belief`) would wrongly be
        // treated as not-yet-live here too, and `missing_guaranteed_status_
        // blockers` would return nothing for it even though the renderer (which
        // DOES thread scratch) correctly emitted the marker. Compute each
        // event's markers against scratch-so-far, THEN fold that event in —
        // same order `render_turn` uses — so the two stay in lockstep.
        let mut scratch = belief.clone();
        for event in original {
            if matches!(event.kind, EventKind::EndOfTurn) {
                fold_event_into_synthesis_scratch(&mut scratch, event, pokemon_dex());
                continue;
            }
            for (target, _) in
                missing_guaranteed_status_blockers(event, &scratch, move_dex(), pokemon_dex())
            {
                let signature = format!("Blocked {{ target: {target:?} }}");
                let position = actual
                    .iter()
                    .position(|value| value == &signature)
                    .unwrap_or_else(|| {
                        panic!("{context}: encoding-only blocker was not recovered for {target:?}")
                    });
                actual.remove(position);
            }
            fold_event_into_synthesis_scratch(&mut scratch, event, pokemon_dex());
        }
        assert_eq!(
            actual, expected,
            "{context}: tracker grammar changed the observable event multiset\n\
             --- tracker text ---\n{text}\n\
             --- original events ---\n{original:#?}\n\
             --- decoded events ---\n{decoded:#?}"
        );
    }

    /// Run one turn's real events (`sample_turn`, observer = P1) through the
    /// actual tracker submission pipeline for P1's belief and assert the
    /// subset oracle holds against the real resulting `BattleState`.
    /// Panics (failing the test) on an unsupported event — for this small,
    /// hand-picked scenario set that should never happen; if it does, the
    /// scenario needs a simpler move choice, not a test-side workaround.
    fn drive_turn(
        belief: UnknownMatchState,
        state: &MatchState,
        p1_cmd: &PlayerCommand,
        p2_cmd: &PlayerCommand,
        true_state_after: &poke_rust::state::battle::BattleState,
        context: &str,
    ) -> UnknownMatchState {
        let (_next_state, events, _prob) = poke_rust::simulator::sample_turn(
            state,
            p1_cmd,
            p2_cmd,
            move_dex(),
            pokemon_dex(),
            true,
            16,
            Some(Player::P1),
        );
        let events = events.expect("observer set — events must be Some");
        let masked = mask_events_for(Player::P1, &events);

        let UnknownMatchState::Battle(fog_before) = &belief else {
            panic!("expected Battle-phase belief")
        };
        let text = render_turn(&masked, fog_before, move_dex(), pokemon_dex())
            .unwrap_or_else(|e| panic!("{context}: renderer hit an unsupported event: {}", e.0));

        let lines =
            crate::tracker_parse::parse_tracker_text(&text, fog_before, move_dex(), pokemon_dex())
                .unwrap_or_else(|e| {
                    panic!(
                        "{context}: re-parsing rendered text failed at line {}: {}\n---\n{text}",
                        e.line, e.message
                    )
                });

        let mut turn_events = Vec::new();
        let mut end_of_turn_reactions = Vec::new();
        for line in lines {
            match line {
                crate::tracker_parse::TrackerLine::Event(ev) => turn_events.push(ev),
                crate::tracker_parse::TrackerLine::EndOfTurnReaction(ev) => {
                    end_of_turn_reactions.push(ev)
                }
                crate::tracker_parse::TrackerLine::EndOfTurn => {
                    turn_events.push(InformationEvent {
                        kind: EventKind::EndOfTurn,
                        reactions: std::mem::take(&mut end_of_turn_reactions),
                    })
                }
            }
        }
        let turn_events = crate::tracker_parse::fold_leads_and_entry_abilities(turn_events);
        let turn_events = crate::tracker_effects::augment_turn(
            turn_events,
            fog_before,
            move_dex(),
            pokemon_dex(),
        );

        let config = InferenceConfig {
            use_stat_points: true,
            force_max_ivs: true,
            ..Default::default()
        };
        let next_belief = apply_information(
            belief,
            &turn_events,
            false,
            pokemon_dex(),
            move_dex(),
            ability_dex(),
            &config,
        );

        assert_true_state_subset_of_belief(
            true_state_after,
            &next_belief,
            Player::P1,
            pokemon_dex(),
            move_dex(),
            context,
        );
        next_belief
    }

    #[test]
    fn hand_authored_tracker_round_trip_stays_a_sound_superset_of_the_real_battle() {
        let preview = poke_rust::simulator::team_preview_state_from_team_strings(
            TEAM_P1,
            TEAM_P2,
            pokemon_dex(),
            move_dex(),
            1,
            1,
            true,
        );
        let tp = TeamPreviewCommand {
            active_indices: vec![0],
            back_indices: vec![],
        };
        let p1_cmd = PlayerCommand::TeamPreview(tp.clone());
        let p2_cmd = PlayerCommand::TeamPreview(tp.clone());

        // Mirrors `create_tracker`'s own belief initialization exactly: go
        // straight to a Battle-phase belief with NOBODY active on either
        // side — a real tracker session never represents team preview at
        // all, the first `leads` turn's `SimultaneousSwitch` events populate
        // the actives through the ordinary switch-handling path.
        let UnknownMatchState::TeamPreview(team_preview_belief) =
            UnknownMatchState::team_preview_closed_sheet_from_perspective(
                Player::P1,
                &preview.p1_mons,
                &preview.p2_mons,
                pokemon_dex(),
                1,
                1,
                50,
                true,
            )
        else {
            panic!("expected TeamPreview variant")
        };
        let all_p1_indices: Vec<usize> = (0..preview.p1_mons.len()).collect();
        let belief = UnknownMatchState::Battle(team_preview_belief.into_battle_state(
            Player::P1,
            &[],
            &all_p1_indices,
            &[],
            &[],
        ));

        // Turn 1: team-preview -> battle transition (leads enter; Sand Stream
        // fires automatically on Tyranitar's send-out).
        let (state_after_leads, _e, _p) = poke_rust::simulator::sample_turn(
            &MatchState::TeamPreviewState(preview.clone()),
            &p1_cmd,
            &p2_cmd,
            move_dex(),
            pokemon_dex(),
            true,
            16,
            Some(Player::P1),
        );
        let MatchState::BattleState(true_battle_1) = state_after_leads.clone() else {
            panic!("expected BattleState after leads")
        };
        let belief = drive_turn(
            belief,
            &MatchState::TeamPreviewState(preview),
            &p1_cmd,
            &p2_cmd,
            &true_battle_1,
            "turn 1 (leads)",
        );

        // Turn 2: P1 Thunder Waves (guaranteed 100% paralysis, no target
        // token needed on the parser side since render_turn always emits
        // one), P2 Tackles back.
        let p1_cmd = PlayerCommand::Battle(vec![BattleCommand::Attack(AttackCommand {
            move_slot: 1,
            target: Some(FieldSlot {
                player: Player::P2,
                slot_index: 0,
            }),
            terastallize: false,
            mega_evolve: false,
        })]);
        let p2_cmd = PlayerCommand::Battle(vec![BattleCommand::Attack(AttackCommand {
            move_slot: 0,
            target: Some(FieldSlot {
                player: Player::P1,
                slot_index: 0,
            }),
            terastallize: false,
            mega_evolve: false,
        })]);
        let (state_after_turn2, _e, _p) = poke_rust::simulator::sample_turn(
            &state_after_leads,
            &p1_cmd,
            &p2_cmd,
            move_dex(),
            pokemon_dex(),
            true,
            16,
            Some(Player::P1),
        );
        let MatchState::BattleState(true_battle_2) = state_after_turn2 else {
            panic!("expected BattleState after turn 2")
        };
        drive_turn(
            belief,
            &state_after_leads,
            &p1_cmd,
            &p2_cmd,
            &true_battle_2,
            "turn 2 (thunder wave + tackle)",
        );
    }

    /// Deterministic full-battle tracker fuzz: genuine simulator events must
    /// survive event rendering, text parsing, lead folding, guaranteed-effect
    /// synthesis, production turn validation/logging, and inference without
    /// reaching a contradiction. Any renderer gap is a failure, not a skipped
    /// turn, and every failure is replayable from its iteration seed.
    #[test]
    fn randomized_tracker_text_round_trips_do_not_contradict() {
        run_tracker_fuzz(false);
    }

    /// Stronger truth-in-belief oracle, matching the inference fuzz suite's
    /// ignored subset sweep. It remains opt-in while the shared inference
    /// engine's known subset-oracle bug families are unresolved.
    #[test]
    #[ignore]
    fn randomized_tracker_text_beliefs_stay_sound_subset() {
        run_tracker_fuzz(true);
    }

    fn run_tracker_fuzz(check_subset: bool) {
        let iterations = fuzz_env_u64("POKERUST_TRACKER_FUZZ_ITERS", 10);
        let seed_start = fuzz_env_u64("POKERUST_TRACKER_FUZZ_SEED_START", 0);
        let max_turns = fuzz_env_u64("POKERUST_TRACKER_FUZZ_MAX_TURNS", 150) as usize;
        let replay = fuzz_env_bool("POKERUST_TRACKER_FUZZ_REPLAY");
        let real_teams = fuzz_env_bool("POKERUST_TRACKER_FUZZ_REAL_TEAMS");
        let continue_on_failure = fuzz_env_bool("POKERUST_TRACKER_FUZZ_CONTINUE");
        let mut failed_seeds = Vec::new();

        for iter in seed_start..seed_start.saturating_add(iterations) {
            let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut rng = StdRng::seed_from_u64(iter);
            let (preview, matchup) = if real_teams {
                let p1_path = REAL_FUZZ_TEAMSHEETS[rng.gen_range(0..REAL_FUZZ_TEAMSHEETS.len())];
                let p2_path = REAL_FUZZ_TEAMSHEETS[rng.gen_range(0..REAL_FUZZ_TEAMSHEETS.len())];
                (
                    poke_rust::simulator::team_preview_state_from_teamsheets(
                        p1_path,
                        p2_path,
                        pokemon_dex(),
                        move_dex(),
                        2,
                        4,
                        true,
                    ),
                    format!("{p1_path} vs {p2_path}"),
                )
            } else {
                (
                    poke_rust::simulator::team_preview_state_from_team_strings(
                        FUZZ_TEAM_P1,
                        FUZZ_TEAM_P2,
                        pokemon_dex(),
                        move_dex(),
                        2,
                        4,
                        true,
                    ),
                    "renderer-complete corpus".to_string(),
                )
            };
            let mut session = fully_benched_tracker_session(&preview);
            let mut state = MatchState::TeamPreviewState(preview.clone());
            let mut p1_cmd = PlayerCommand::TeamPreview(random_team_preview_command(
                preview.p1_mons.len(),
                &mut rng,
            ));
            let mut p2_cmd = PlayerCommand::TeamPreview(random_team_preview_command(
                preview.p2_mons.len(),
                &mut rng,
            ));
            let mut pending_masked: Vec<InformationEvent> = Vec::new();
            let mut pending_raw: Vec<InformationEvent> = Vec::new();
            let mut pending_commands: Vec<(PlayerCommand, PlayerCommand)> = Vec::new();
            let mut logical_turn = 1usize;

            for phase in 1..=max_turns {
                let context = format!(
                    "tracker_fuzz seed={iter} turn={logical_turn} phase={phase} matchup={matchup} p1_cmd={p1_cmd:?} p2_cmd={p2_cmd:?}"
                );
                let sample_seed = iter
                    .wrapping_mul(TRACKER_FUZZ_SAMPLE_SALT)
                    .wrapping_add(phase as u64);
                let was_team_preview = matches!(state, MatchState::TeamPreviewState(_));
                let (next_state, raw_events, _) = poke_rust::simulator::sample_turn_raw_seeded(
                    sample_seed,
                    &state,
                    &p1_cmd,
                    &p2_cmd,
                    move_dex(),
                    pokemon_dex(),
                    true,
                    16,
                    Some(Player::P1),
                );
                let raw_events = raw_events.expect("observer enabled");
                let masked = mask_events_for(Player::P1, &raw_events);
                pending_commands.push((p1_cmd.clone(), p2_cmd.clone()));
                pending_raw.extend(raw_events);
                let has_end_of_turn = masked
                    .iter()
                    .any(|event| matches!(event.kind, EventKind::EndOfTurn));
                pending_masked.extend(masked);
                state = next_state;
                let game_over = matches!(state, MatchState::GameOverState { .. });
                let turn_complete = was_team_preview || has_end_of_turn || game_over;

                if !turn_complete {
                    let MatchState::BattleState(true_state) = &state else {
                        panic!("{context}: incomplete simulator phase returned a non-battle state")
                    };
                    p1_cmd = PlayerCommand::Battle(random_commands_for_player(
                        true_state,
                        Player::P1,
                        &mut rng,
                    ));
                    p2_cmd = PlayerCommand::Battle(random_commands_for_player(
                        true_state,
                        Player::P2,
                        &mut rng,
                    ));
                    continue;
                }

                let raw_events = std::mem::take(&mut pending_raw);
                let masked = std::mem::take(&mut pending_masked);
                let command_phases = std::mem::take(&mut pending_commands);
                let text = render_turn(
                    &masked,
                    &session.belief,
                    move_dex(),
                    pokemon_dex(),
                )
                .unwrap_or_else(|error| {
                    panic!(
                        "{context}: unsupported tracker rendering: {}\nraw={raw_events:#?}\nmasked={masked:#?}",
                        error.0
                    )
                });
                let decoded = decode_rendered_turn(&text, &session.belief);
                assert_event_round_trip(&masked, &decoded, &session.belief, &context, &text);
                let text = append_explicit_passes(text, &command_phases, &masked, game_over);
                if replay {
                    eprintln!(
                        "[TRACKER-FUZZ] {context}\n--- tracker text ---\n{text}\n--- raw events ---\n{raw_events:#?}\n--- masked events ---\n{masked:#?}"
                    );
                }

                let log_before = session.log.len();
                let response = apply_tracker_text(&mut session, &text, dexes()).unwrap_or_else(
                    |error| {
                        panic!(
                            "{context}: production tracker pipeline rejected rendered text: {error:?}\n--- tracker text ---\n{text}\n--- raw events ---\n{raw_events:#?}\n--- masked events ---\n{masked:#?}"
                        )
                    },
                );
                assert_eq!(session.log.len(), log_before + 1, "{context}");
                assert_eq!(response.log_delta.len(), 1, "{context}");
                let expected_view = crate::mapping::battle_view_from_belief(
                    &session.belief,
                    session.active_per_side,
                    session.brought_per_side,
                    session.inference_config.legal_items.as_ref(),
                );
                assert_eq!(
                    serde_json::to_value(&response.state).unwrap(),
                    serde_json::to_value(&expected_view).unwrap(),
                    "{context}: response view drifted from the committed belief"
                );

                match &state {
                    MatchState::GameOverState { .. } => break,
                    MatchState::BattleState(true_state) => {
                        if check_subset {
                            let belief = UnknownMatchState::Battle(session.belief.clone());
                            assert_true_state_subset_of_belief(
                                true_state,
                                &belief,
                                Player::P1,
                                pokemon_dex(),
                                move_dex(),
                                &context,
                            );
                            // The ordinary subset sweep checks only the observer's
                            // opponent. Running the physical inverse here also
                            // guards the tracker's fully-known own side.
                            assert_true_state_subset_of_belief(
                                true_state,
                                &belief,
                                Player::P2,
                                pokemon_dex(),
                                move_dex(),
                                &format!("{context} own-side"),
                            );
                        }
                        p1_cmd = PlayerCommand::Battle(random_commands_for_player(
                            true_state,
                            Player::P1,
                            &mut rng,
                        ));
                        p2_cmd = PlayerCommand::Battle(random_commands_for_player(
                            true_state,
                            Player::P2,
                            &mut rng,
                        ));
                    }
                    MatchState::TeamPreviewState(_) => unreachable!(),
                }
                logical_turn += 1;

                if phase == max_turns {
                    eprintln!(
                        "{context}: battle exceeded the fuzz phase guard; stopping this seed"
                    );
                    break;
                }
            }
            }));
            if let Err(payload) = run {
                if continue_on_failure {
                    failed_seeds.push(iter);
                    continue;
                }
                std::panic::resume_unwind(payload);
            }
        }
        if !failed_seeds.is_empty() {
            panic!(
                "tracker fuzz completed the requested range with failing seeds: {failed_seeds:?}"
            );
        }
    }
}
