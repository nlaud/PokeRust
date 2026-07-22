//! The inverse of `tracker_parse.rs`: renders a real (masked) engine event
//! stream back into tracker-syntax text, for the round-trip fidelity fuzz
//! test (see `tracker_render_fuzz` below and the plan this implements —
//! "if I have a simulated run and manually input the events that are
//! happening into the tracker then it should be accurately tracking the
//! state of the game").
//!
//! # Scope
//!
//! The tracker's own grammar is a deliberately-scoped "Phase 1 MVP" subset
//! (see `tracker_parse.rs`'s module doc) — it cannot express every
//! `EventKind` the engine can produce (no grammar exists yet for e.g.
//! `SlotConditionStart`/`FormeChange`/`Transformed`/`IllusionEnded`). This
//! renderer covers the common, high-value subset a real tracker session
//! actually exercises (moves with damage/status/boost/volatile secondaries,
//! switches, weather/terrain, abilities, items, mega evolution,
//! Terastallization) and returns `Err` for anything else, rather than
//! guessing at unsupported syntax. The randomized gating sweep uses a
//! renderer-complete battle corpus and fails on an unsupported event;
//! unsupported engine vocabulary remains an explicit grammar-expansion task.
//!
//! # Avoiding double-application
//!
//! A real user never re-types a move's *guaranteed* consequences — Thunder
//! Wave's paralysis, Stealth Rock's hazard, an ability's on-reveal effect —
//! because `tracker_effects::augment_turn` re-synthesizes them automatically
//! once the text is re-parsed. If this renderer echoed those reactions back
//! as literal tokens too, re-augmenting the rendered text would apply them
//! TWICE (double boosts, re-set statuses, etc.) — unsound and unrealistic.
//! `reactions_requiring_explicit_render` closes this gap: it diffs a real
//! event's reactions against what `augment_with_guaranteed_effects` would
//! synthesize for a bare (reaction-less) clone of the same event, and only
//! the leftover (non-synthesizable) reactions get rendered as explicit text
//! — exactly mirroring how a real tracker user only types what they'd
//! actually have to type by hand.
//!
//! Not yet wired into any HTTP route — today this module drives the
//! hand-authored and randomized tracker-fidelity tests below, hence the
//! blanket `dead_code` allow (a normal, non-test build has no other caller).
//! A natural next step is exposing this as a real
//! "narrate this simulated battle as tracker text" feature.
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

use crate::tracker_effects::augment_with_guaranteed_effects;

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
    for event in events {
        if matches!(event.kind, EventKind::EndOfTurn) {
            for reaction in &event.reactions {
                render_standalone_tree(reaction, belief, &mut lines)?;
            }
            continue; // the sentinel itself is added once below.
        }
        if let Some(line) = render_top_level_event(event, belief, move_dex, pokemon_dex)? {
            lines.push(line);
        }
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
        EventKind::DamageDealt { target, new_hp, .. }
        | EventKind::Healed { target, new_hp, .. }
        | EventKind::SetHp { target, new_hp, .. } => lines.push(format!(
            "{} hp {}",
            slot_token(*target),
            raw_hp_token(new_hp)
        )),
        EventKind::Faint { .. } => {}
        EventKind::AbilityRevealed { .. }
        | EventKind::ItemRevealed { .. }
        | EventKind::WeatherChanged { .. }
        | EventKind::TerrainChanged { .. } => lines.push(render_standalone_event(event, belief)?),
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
    let explicit_reactions = reactions_requiring_explicit_render(event, belief, move_dex, pokemon_dex);

    match &event.kind {
        EventKind::MoveUsed { user, move_used, targets } => {
            let mut line = format!("{} {}", slot_token(*user), move_word(move_used));
            for &target in targets {
                let _ = write!(line, " {}", slot_token(target));
            }
            for r in &explicit_reactions {
                let _ = write!(
                    line,
                    " {}",
                    render_move_qualifier(r, belief, move_dex, pokemon_dex)?
                );
            }
            for qualifier in missing_guaranteed_status_blockers(
                event,
                belief,
                move_dex,
                pokemon_dex,
            ) {
                let _ = write!(line, " {qualifier}");
            }
            Ok(Some(line))
        }
        EventKind::Switch(sw) => Ok(Some(format!(
            "{} switch {} {}",
            slot_token(sw.slot),
            species_word(&sw.species),
            hp_token(&sw.hp)
        ))),
        EventKind::SimultaneousSwitch { switches } => {
            // Rendered as separate per-side `leads` lines (mirroring how a
            // user types them) — `fold_leads_and_entry_abilities` re-merges
            // them on the parse side. Entry-ability reveals nested as
            // reactions are rendered as their own follow-up lines; their
            // guaranteed cascades are re-synthesized by `augment_turn`, same
            // diffing discipline as everywhere else.
            let mut out = Vec::new();
            for player in [Player::P1, Player::P2] {
                let mut side: Vec<&SwitchState> =
                    switches.iter().filter(|sw| sw.slot.player == player).collect();
                if side.is_empty() {
                    continue;
                }
                // Simulator action order is speed/queue order, not slot order;
                // `leads` assigns species left-to-right to slots 0, 1, ... .
                side.sort_by_key(|sw| sw.slot.slot_index);
                let fills_from_left = side
                    .iter()
                    .enumerate()
                    .all(|(index, sw)| sw.slot.slot_index as usize == index);
                if fills_from_left {
                    let species_list = side
                        .iter()
                        .map(|sw| species_word(&sw.species))
                        .collect::<Vec<_>>()
                        .join(" ");
                    out.push(format!("{} leads {}", side_word(player), species_list));
                } else {
                    // A simultaneous post-faint replacement may fill only a
                    // subset of doubles slots. `leads` cannot preserve those
                    // indices, so emit ordinary slot-addressed switches.
                    out.extend(side.iter().map(|sw| {
                        format!(
                            "{} switch {} {}",
                            slot_token(sw.slot),
                            species_word(&sw.species),
                            hp_token(&sw.hp)
                        )
                    }));
                }
            }
            for r in &explicit_reactions {
                out.push(render_standalone_event(r, belief)?);
            }
            Ok(Some(out.join("\n")))
        }
        EventKind::MegaEvolution { slot, into } => {
            Ok(Some(format!("{} mega {}", slot_token(*slot), species_word(into))))
        }
        EventKind::Terastallization { slot, tera_type } => Ok(Some(format!(
            "{} tera {}",
            slot_token(*slot),
            type_word(tera_type)
        ))),
        EventKind::Cant { slot, reason } => {
            Ok(Some(format!("{} {}", slot_token(*slot), cant_reason_word(reason)?)))
        }
        EventKind::MustRecharge { slot } => Ok(Some(format!("{} mustrecharge", slot_token(*slot)))),
        EventKind::AbilityRevealed { .. } | EventKind::ItemRevealed { .. } => {
            render_standalone_event(event, belief).map(Some)
        }
        // Auto-synthesized as a sibling of any zero-HP DamageDealt/Healed/SetHp
        // (`synthesize_guaranteed_faints`) — never rendered directly.
        EventKind::Faint { .. } => Ok(None),
        other => Err(unsupported(format!("{other:?} has no top-level tracker grammar yet"))),
    }
}

fn missing_guaranteed_status_blockers(
    event: &InformationEvent,
    belief: &UnknownBattleState,
    move_dex: &HashMap<PokemonMove, MoveData>,
    pokemon_dex: &HashMap<Species, PokemonData>,
) -> Vec<String> {
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
            (!observed && !already_failed).then(|| format!("{} blocked", slot_token(*target)))
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
        EventKind::WeatherChanged { weather: Some(w) } => Ok(format!("weather {}", weather_word(w))),
        EventKind::WeatherChanged { weather: None } => Ok("weather none".to_string()),
        EventKind::TerrainChanged { terrain: Some(t) } => Ok(format!("terrain {}", terrain_word(t))),
        EventKind::TerrainChanged { terrain: None } => Ok("terrain none".to_string()),
        other => Err(unsupported(format!("{other:?} has no standalone tracker grammar yet"))),
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
        EventKind::DamageDealt { target, new_hp, .. } => {
            Ok(format!("{} {}", slot_token(*target), raw_hp_token(new_hp)))
        }
        EventKind::Healed { target, new_hp, .. } => {
            Ok(format!("{} {}", slot_token(*target), raw_hp_token(new_hp)))
        }
        EventKind::SetHp { target, new_hp, .. } => {
            Ok(format!("{} {}", slot_token(*target), raw_hp_token(new_hp)))
        }
        EventKind::StatusInflicted { target, status } => {
            Ok(format!("{} {}", slot_token(*target), status_word(status)?))
        }
        EventKind::VolatileStart { target, volatile } => {
            Ok(format!("{} {}", slot_token(*target), volatile_word(volatile)?))
        }
        EventKind::BoostChanged { target, boost_idx, stages } => {
            Ok(format!("{} {}", slot_token(*target), boost_token(*boost_idx, *stages)?))
        }
        EventKind::ItemLost { slot, item, consumed } => Ok(format!(
            "{} {} {}",
            slot_token(*slot),
            if *consumed { "consumes" } else { "loses" },
            item_word(item)
        )),
        EventKind::AbilityRevealed { slot, ability } => {
            Ok(format!("{} {}", slot_token(*slot), ability_word(ability)))
        }
        // Field-level payloads of a move's own secondaries — rendered as a
        // standalone-style word appended inline; the target-agnostic ones
        // (weather/terrain) don't need a slot prefix on a move line since
        // they're global/side-scoped, not per-mon.
        EventKind::WeatherChanged { weather: Some(w) } => Ok(weather_word(w).to_string()),
        EventKind::TerrainChanged { terrain: Some(t) } => Ok(terrain_word(t).to_string()),
        other => return Err(unsupported(format!(
            "{other:?} has no inline move-qualifier tracker grammar yet"
        ))),
    }?;

    // The simulator often nests observations below their trigger (Crit below
    // DamageDealt, recoil below an AbilityRevealed, berry healing below
    // ItemLost). Tracker syntax is intentionally flat, so recursively append
    // only children that the production augmenter will not recreate.
    for reaction in
        reactions_requiring_explicit_render(event, belief, move_dex, pokemon_dex)
    {
        let _ = write!(
            rendered,
            " {}",
            render_move_qualifier(reaction, belief, move_dex, pokemon_dex)?
        );
    }
    Ok(rendered)
}

/// Reactions of `event` that `augment_with_guaranteed_effects` would NOT
/// already re-synthesize on its own for a bare (reaction-less) clone — see
/// this module's doc comment for why this diff is necessary. Only compares
/// one level deep (direct children), matching the shapes
/// `augment_with_guaranteed_effects` itself produces for `MoveUsed`/
/// `MegaEvolution`/`AbilityRevealed` (their synthesized reactions are leaves
/// or, for a synthesized `AbilityRevealed`, a fully-cascaded subtree that
/// compares equal as a whole via `PartialEq`).
fn reactions_requiring_explicit_render<'a>(
    event: &'a InformationEvent,
    belief: &UnknownBattleState,
    move_dex: &HashMap<PokemonMove, MoveData>,
    pokemon_dex: &HashMap<Species, PokemonData>,
) -> Vec<&'a InformationEvent> {
    let bare = InformationEvent { kind: event.kind.clone(), reactions: Vec::new() };
    let synthetic = augment_with_guaranteed_effects(bare, belief, move_dex, pokemon_dex);
    let mut remaining_synthetic = synthetic.reactions;
    event
        .reactions
        .iter()
        .filter(|r| {
            if let EventKind::Faint { slot } = &r.kind
                && event.reactions.iter().any(|sibling| match &sibling.kind {
                    EventKind::DamageDealt { target, new_hp, .. }
                    | EventKind::Healed { target, new_hp, .. }
                    | EventKind::SetHp { target, new_hp, .. } => {
                        target == slot
                            && matches!(new_hp, PokemonHP::Number(0) | PokemonHP::Percent(0))
                    }
                    _ => false,
                })
            {
                return false;
            }
            if matches!(r.kind, EventKind::Faint { .. })
                && matches!(
                    &event.kind,
                    EventKind::DamageDealt { new_hp: PokemonHP::Number(0) | PokemonHP::Percent(0), .. }
                        | EventKind::Healed { new_hp: PokemonHP::Number(0) | PokemonHP::Percent(0), .. }
                        | EventKind::SetHp { new_hp: PokemonHP::Number(0) | PokemonHP::Percent(0), .. }
                )
            {
                return false;
            }
            // Protect-style resolution emits this bookkeeping child even
            // though the enclosing MoveUsed already records the same user and
            // move. The tracker parser intentionally represents the action
            // once; inference treats MoveUsed as the same committed action.
            if let (
                EventKind::MoveUsed { user, move_used, .. },
                EventKind::SingleMoveOrTurn { slot, move_used: child_move },
            ) = (&event.kind, &r.kind)
                && user == slot
                && move_used == child_move
            {
                return false;
            }
            if let Some(pos) = remaining_synthetic.iter().position(|s| s == *r) {
                remaining_synthetic.remove(pos);
                false
            } else {
                true
            }
        })
        .collect()
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
        other => return Err(unsupported(format!("{other:?} has no volatile tracker word yet"))),
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
        other => return Err(unsupported(format!("{other:?} has no cant-reason tracker word yet"))),
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
    use poke_rust::information::inference::{InferenceConfig, apply_information};
    use poke_rust::information::information::mask_events_for;
    use poke_rust::information::subset_check::assert_true_state_subset_of_belief;
    use poke_rust::information::unknowns::UnknownMatchState;
    use poke_rust::state::battle::{
        AttackCommand, BattleCommand, BattleState, MatchState, PlayerCommand, TeamPreviewCommand,
    };
    use poke_rust::state::dex_data::{parse_ability_dex, parse_move_dex, parse_pokemon_dex, AbilityData};
    use std::sync::OnceLock;
    use rand::rngs::StdRng;
    use rand::{Rng, SeedableRng};
    use crate::session::Dexes;
    use crate::tracker::{TrackerSession, apply_tracker_text};

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
            poke_rust::state::dex_data::parse_learnset_dex(
                "../pokemon_info/showdownLearnsets.txt",
            )
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
                .map(|slot_options| {
                    slot_options[rng.gen_range(0..slot_options.len())].clone()
                })
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
        TrackerSession {
            belief: team_preview_belief.into_battle_state(
                Player::P1,
                &[],
                &all_p1,
                &[],
                &[],
            ),
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
        }
    }

    fn append_explicit_passes(
        mut text: String,
        p1_cmd: &PlayerCommand,
        p2_cmd: &PlayerCommand,
        events: &[InformationEvent],
        game_over: bool,
    ) -> String {
        let mut passes = Vec::new();
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
                        EventKind::Cant { slot: event_slot, .. }
                        | EventKind::MustRecharge { slot: event_slot } => *event_slot == slot,
                        _ => false,
                    });
                    if matches!(command, BattleCommand::Pass) || (game_over && !acted) {
                        passes.push(format!(
                            "{}{} pass",
                            side_word(player),
                            slot_index + 1
                        ));
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
        text.push_str(&passes.join("\n"));
        text.push('\n');
        text.push_str(sentinel);
        text
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

        let lines = crate::tracker_parse::parse_tracker_text(&text, fog_before, move_dex(), pokemon_dex())
            .unwrap_or_else(|e| panic!("{context}: re-parsing rendered text failed at line {}: {}\n---\n{text}", e.line, e.message));

        let mut turn_events = Vec::new();
        for line in lines {
            match line {
                crate::tracker_parse::TrackerLine::Event(ev) => turn_events.push(ev),
                crate::tracker_parse::TrackerLine::EndOfTurn => {
                    turn_events.push(InformationEvent { kind: EventKind::EndOfTurn, reactions: Vec::new() })
                }
            }
        }
        let turn_events = crate::tracker_parse::fold_leads_and_entry_abilities(turn_events);
        let turn_events = crate::tracker_effects::augment_turn(turn_events, fog_before, move_dex(), pokemon_dex());

        let config = InferenceConfig { use_stat_points: true, force_max_ivs: true, ..Default::default() };
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
            TEAM_P1, TEAM_P2, pokemon_dex(), move_dex(), 1, 1, true,
        );
        let tp = TeamPreviewCommand { active_indices: vec![0], back_indices: vec![] };
        let p1_cmd = PlayerCommand::TeamPreview(tp.clone());
        let p2_cmd = PlayerCommand::TeamPreview(tp.clone());

        // Mirrors `create_tracker`'s own belief initialization exactly: go
        // straight to a Battle-phase belief with NOBODY active on either
        // side — a real tracker session never represents team preview at
        // all, the first `leads` turn's `SimultaneousSwitch` events populate
        // the actives through the ordinary switch-handling path.
        let UnknownMatchState::TeamPreview(team_preview_belief) =
            UnknownMatchState::team_preview_closed_sheet_from_perspective(
                Player::P1, &preview.p1_mons, &preview.p2_mons, pokemon_dex(), 1, 1, 50, true,
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
            target: Some(FieldSlot { player: Player::P2, slot_index: 0 }),
            terastallize: false,
            mega_evolve: false,
        })]);
        let p2_cmd = PlayerCommand::Battle(vec![BattleCommand::Attack(AttackCommand {
            move_slot: 0,
            target: Some(FieldSlot { player: Player::P1, slot_index: 0 }),
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

        for iter in seed_start..seed_start.saturating_add(iterations) {
            let mut rng = StdRng::seed_from_u64(iter);
            let preview = poke_rust::simulator::team_preview_state_from_team_strings(
                FUZZ_TEAM_P1,
                FUZZ_TEAM_P2,
                pokemon_dex(),
                move_dex(),
                2,
                4,
                true,
            );
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

            for turn in 1..=max_turns {
                let context = format!(
                    "tracker_fuzz seed={iter} turn={turn} p1_cmd={p1_cmd:?} p2_cmd={p2_cmd:?}"
                );
                let sample_seed = iter
                    .wrapping_mul(TRACKER_FUZZ_SAMPLE_SALT)
                    .wrapping_add(turn as u64);
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
                let text = append_explicit_passes(
                    text,
                    &p1_cmd,
                    &p2_cmd,
                    &masked,
                    matches!(next_state, MatchState::GameOverState { .. }),
                );
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

                state = next_state;
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

                if turn == max_turns {
                    eprintln!("{context}: battle exceeded the fuzz hang guard; stopping this seed");
                    break;
                }
            }
        }
    }
}
