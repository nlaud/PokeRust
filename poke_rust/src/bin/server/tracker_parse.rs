//! Tracker mode: parses free-text descriptions of a real battle's events into
//! the `InformationEvent` trees `apply_information` expects.
//!
//! # Scope (Phase 1 MVP)
//!
//! This is a deliberately-scoped subset of the full grammar described in the
//! tracker-mode design doc, chosen to get a sound, working pipeline in place
//! first. Notable simplifications (documented inline at their call sites too):
//!
//! - **Flat nesting.** All effects on a move line become *direct children* of
//!   the `MoveUsed` node (siblings), keyed to whichever slot was last
//!   mentioned, rather than the fully cause-nested trees the simulator itself
//!   emits (e.g. a berry heal nested three levels under the `DamageDealt` /
//!   `ItemLost` that triggered it). Inference reasons about the *presence* of
//!   node types within a `MoveUsed` subtree, not exact parent/child shape, so
//!   this stays sound; it is just less legible than simulator output would be.
//! - **Explicit targets required.** Every targeted move must name its
//!   target slot(s) explicitly (matches every example in the design doc); no
//!   singles auto-target inference. The one exception is a **charge turn**
//!   (`o1 solarbeam charging`): the charge step frequently reveals no target at
//!   all — "Charizard flew up high!" names nobody — so `charging` lines are
//!   allowed to carry an empty target list, and the release turn a turn later
//!   records the target normally. A target may still be given if it IS known.
//! - **Leads are an event, not a pre-game pick.**
//!   `leads [p|o] <species>... [p|o] <species>...` sends out one or both
//!   sides' opening (or simultaneous post-faint replacement) leads together
//!   — a session starts fully benched on both sides (see `tracker.rs`'s
//!   module doc), symmetric with how every other mid-battle switch already
//!   works. Distinct from `switch`, which replaces one slot at a time. A
//!   single `leads` line covering both sides parses directly to ONE
//!   `SimultaneousSwitch` event; if a submission instead spells the two
//!   sides out as separate consecutive `leads` lines,
//!   `fold_leads_and_entry_abilities` (called once per turn, before
//!   synthesis/inference) still merges that leading run into ONE combined
//!   event and folds any immediately-following entry-ability reveal
//!   (`p1 sandstream`, `o1 unnerve`, …) into that event's `reactions` — see
//!   its doc comment for why this matters beyond tidiness (cross-mon
//!   ability-absence reasoning).
//! - **HP direction from the belief.** `[xx]%`/`[xx]hp` tokens don't say
//!   whether they're damage or healing — that's inferred by comparing against
//!   the slot's currently-believed HP. Equal-to-current is emitted as `SetHp`
//!   (no mechanism implied).
//! - Guaranteed-effect synthesis (Intimidate's `-1 atk`, Swords Dance's
//!   `+2 atk`, weather from Drizzle, …) lives in `tracker_effects.rs` and is
//!   applied as a post-processing pass over the events this module builds —
//!   see `crate::tracker_effects::augment_turn`.

use std::collections::HashMap;

use poke_rust::data::ability::Ability;
use poke_rust::data::item::Item;
use poke_rust::data::pokemon_move::PokemonMove;
use poke_rust::data::species::Species;
use poke_rust::information::information::{CantReason, EventKind, InformationEvent, SwitchState};
use poke_rust::information::unknowns::{PokemonHP, UnknownBattleState};
use poke_rust::state::battle::{FieldSlot, Player};
use poke_rust::state::dex_data::{
    MoveData, PokemonData, PokemonType, PseudoWeather, SideCondition, Status, Terrain,
    VolatileStatus, Weather,
};

#[derive(Debug, Clone)]
pub struct ParseError {
    /// 1-based line number within the submitted text.
    pub line: usize,
    pub message: String,
}

/// One parsed line: either a concrete event to fold into this turn, or the
/// `EndOfTurn` marker that commits everything accumulated so far.
#[derive(Debug)]
pub enum TrackerLine {
    Event(InformationEvent),
    /// A reaction belonging to the terminal `EndOfTurn` node.
    EndOfTurnReaction(InformationEvent),
    EndOfTurn,
}

fn hp_readings_from_belief(belief: &UnknownBattleState) -> HashMap<FieldSlot, PokemonHP> {
    belief
        .p1_active_mons
        .iter()
        .enumerate()
        .map(|(index, mon)| {
            (
                FieldSlot {
                    player: Player::P1,
                    slot_index: index as u8,
                },
                mon.hp.clone(),
            )
        })
        .chain(
            belief
                .p2_active_mons
                .iter()
                .enumerate()
                .map(|(index, mon)| {
                    (
                        FieldSlot {
                            player: Player::P2,
                            slot_index: index as u8,
                        },
                        mon.hp.clone(),
                    )
                }),
        )
        .collect()
}

/// Every active slot whose species is currently `Known` in `belief`, seeding
/// the running `slot_species` scratch `parse_tracker_text` threads through a
/// submission — see that function's doc comment for why a running scratch is
/// needed at all (same-turn `leads`/`switch` lines aren't reflected in
/// `belief` itself, which is read-only for the whole submission).
fn slot_species_from_belief(belief: &UnknownBattleState) -> HashMap<FieldSlot, Species> {
    let mut out = HashMap::new();
    for (player, mons) in [
        (Player::P1, &belief.p1_active_mons),
        (Player::P2, &belief.p2_active_mons),
    ] {
        for (index, mon) in mons.iter().enumerate() {
            if let poke_rust::information::unknowns::Unknown::Known(species) = &mon.possible_species {
                out.insert(
                    FieldSlot {
                        player,
                        slot_index: index as u8,
                    },
                    species.clone(),
                );
            }
        }
    }
    out
}

/// Parse every line of `text` into `TrackerLine`s. Blank lines and lines
/// starting with `#` are ignored. `belief` is read (never mutated) to resolve
/// HP-direction (damage vs. heal vs. unchanged) for `hpspec` tokens **and**
/// which species currently occupies a slot for `mega`'s auto-fill/suffix
/// resolution (`active_species_at`) — but `belief` itself is frozen for the
/// whole submission, so a `leads`/`switch` line earlier in the SAME
/// submission (typically the same turn) wouldn't otherwise be visible to a
/// later `mega` line addressing the mon it just sent out. `slot_species`
/// fixes that: seeded from `belief`, then updated in place by `leads` and
/// `switch` as they parse, exactly like `hp_readings` already threads the
/// latest HP reading forward.
pub fn parse_tracker_text(
    text: &str,
    belief: &UnknownBattleState,
    move_dex: &HashMap<PokemonMove, MoveData>,
    pokemon_dex: &HashMap<Species, PokemonData>,
) -> Result<Vec<TrackerLine>, ParseError> {
    let mut out = Vec::new();
    // HP direction is contextual. Thread the latest literal reading through
    // the submission so a switch followed by damage, consecutive multi-hit
    // readings, and later turns in the same batch compare against the event
    // immediately before them rather than the pre-submission belief forever.
    let mut hp_readings = hp_readings_from_belief(belief);
    let mut slot_species = slot_species_from_belief(belief);
    for (idx, raw_line) in text.lines().enumerate() {
        let line_no = idx + 1;
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let tokens: Vec<&str> = line.split_whitespace().collect();
        out.push(parse_line(
            &tokens,
            line_no,
            belief,
            move_dex,
            pokemon_dex,
            &mut hp_readings,
            &mut slot_species,
        )?);
    }
    Ok(out)
}

/// Fold one turn's raw parsed events into their final nested shape before
/// synthesis/inference sees them: every `SimultaneousSwitch` event in the
/// turn's leading contiguous run (i.e. the `leads` line(s) at the very start
/// of the turn — normally just one combined `leads p ... o ...` line, but a
/// submission that instead spells the two sides out as separate consecutive
/// `leads p ...` / `leads o ...` lines is folded the same way) is merged into
/// a SINGLE combined event covering every entering mon on both sides, and any
/// bare `AbilityRevealed` line immediately following that run, addressed to
/// one of the just-entered slots, is moved from its own top-level line into
/// the combined event's `reactions` instead of staying an unrelated sibling.
///
/// This matters for more than just tidiness: `EventKind::SimultaneousSwitch`'s
/// doc comment says ability activations "are nested in this event's
/// `reactions`... exactly as the simulator processes them", and
/// `pass1_ability_absence_inference` (`information/inference.rs`) reads
/// exactly that field to reason across every mon that entered together (e.g.
/// "no weather change appeared, and this is the only entering mon that COULD
/// have a weather setter, so it doesn't have one"). Two separate `leads`
/// lines (one per side) would otherwise produce two independent
/// `SimultaneousSwitch` events with empty `reactions`, so that cross-mon
/// reasoning would only ever see half the field; and a following
/// `p1 sandstream` line would stay a disconnected sibling event instead of
/// the nested reveal the engine expects, silently discarding the
/// entry-ability bookkeeping the design doc describes.
///
/// The fold stops (and leaves everything after it untouched) at the first
/// event that isn't itself a `SimultaneousSwitch` or a qualifying
/// `AbilityRevealed` — a move, an ordinary `switch`, etc. — so an unrelated,
/// later same-turn ability reveal for one of the leads' slots (from a
/// completely different trigger) is never mistakenly folded in as an entry
/// effect.
pub fn fold_leads_and_entry_abilities(events: Vec<InformationEvent>) -> Vec<InformationEvent> {
    let mut result: Vec<InformationEvent> = Vec::with_capacity(events.len());
    let mut merged_switches: Vec<SwitchState> = Vec::new();
    let mut still_entered: std::collections::HashSet<FieldSlot> = std::collections::HashSet::new();
    let mut combined_reactions: Vec<InformationEvent> = Vec::new();
    let mut combined_index: Option<usize> = None;
    let mut in_leads_block = false;

    for ev in events {
        match ev.kind {
            EventKind::SimultaneousSwitch { switches }
                if combined_index.is_none() || in_leads_block =>
            {
                still_entered.extend(switches.iter().map(|sw| sw.slot));
                merged_switches.extend(switches);
                if combined_index.is_none() {
                    combined_index = Some(result.len());
                    // Placeholder — filled in once the whole leading run is known.
                    result.push(InformationEvent {
                        kind: EventKind::SimultaneousSwitch {
                            switches: Vec::new(),
                        },
                        reactions: Vec::new(),
                    });
                }
                in_leads_block = true;
            }
            EventKind::AbilityRevealed { slot, ability }
                if in_leads_block && still_entered.contains(&slot) =>
            {
                combined_reactions.push(InformationEvent {
                    kind: EventKind::AbilityRevealed { slot, ability },
                    reactions: ev.reactions,
                });
            }
            _ => {
                in_leads_block = false;
                result.push(ev);
            }
        }
    }

    if let Some(i) = combined_index {
        result[i] = InformationEvent {
            kind: EventKind::SimultaneousSwitch {
                switches: merged_switches,
            },
            reactions: combined_reactions,
        };
    }

    result
}

// ── Identifier normalization ────────────────────────────────────────────────

/// Case/punctuation-insensitive compare: strips everything but alphanumerics
/// and lowercases, so `SwordsDance`, `swords_dance`, `swords-dance`, and
/// `swordsDance` all compare equal. Mirrors the normalization `Species::from_str`
/// / `PokemonMove::from_str` / `Item::from_str` / `Ability::from_str` already do
/// internally, so tokens can be handed to those functions directly.
fn norm(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

// ── Slot addressing ─────────────────────────────────────────────────────────

/// `p{n}` = the tracker viewer's own side (always physical `Player::P1` — the
/// tracker only ever tracks from one real person's point of view); `o{n}` = the
/// opponent (`Player::P2`). `n` is 1-based, left-to-right; omitted in singles.
fn parse_slot(tok: &str) -> Option<FieldSlot> {
    let n = norm(tok);
    let (player, rest) = if let Some(rest) = n.strip_prefix('p') {
        (Player::P1, rest)
    } else if let Some(rest) = n.strip_prefix('o') {
        (Player::P2, rest)
    } else {
        return None;
    };
    let slot_index = if rest.is_empty() {
        0
    } else {
        rest.parse::<u8>().ok()?.checked_sub(1)?
    };
    Some(FieldSlot { player, slot_index })
}

/// Whole-side marker for the `leads` line — same accepted words as the
/// `side` keyword (`p`/`p1`/`player`, `o`/`o1`/`opponent`); a bare `p`/`o`
/// only ever means "this whole side" here, never a specific slot digit,
/// since `leads` addresses every one of a side's slots at once.
fn leads_side_marker(tok: &str) -> Option<Player> {
    match norm(tok).as_str() {
        "p" | "p1" | "player" => Some(Player::P1),
        "o" | "o1" | "opponent" => Some(Player::P2),
        _ => None,
    }
}

pub(crate) fn opposing_active_slots(
    belief: &UnknownBattleState,
    slot: FieldSlot,
) -> Vec<FieldSlot> {
    let (opp, mons) = match slot.player {
        Player::P1 => (Player::P2, &belief.p2_active_mons),
        Player::P2 => (Player::P1, &belief.p1_active_mons),
    };
    mons.iter()
        .enumerate()
        .filter(|(_, mon)| {
            !mon.fainted && !matches!(mon.hp, PokemonHP::Number(0) | PokemonHP::Percent(0))
        })
        .map(|(index, _)| FieldSlot {
            player: opp,
            slot_index: index as u8,
        })
        .collect()
}

// ── hpspec ───────────────────────────────────────────────────────────────────

enum HpToken {
    Percent(u8),
    Number(u16),
}

fn parse_hp_token(tok: &str) -> Option<HpToken> {
    let lower = tok.to_ascii_lowercase();
    if let Some(digits) = lower.strip_suffix('%') {
        return digits.parse::<u8>().ok().map(HpToken::Percent);
    }
    if let Some(digits) = lower.strip_suffix("hp") {
        return digits.parse::<u16>().ok().map(HpToken::Number);
    }
    None
}

fn parse_hit_count_token(tok: &str) -> Option<u8> {
    let normalized = norm(tok);
    normalized
        .strip_suffix("hits")
        .or_else(|| normalized.strip_suffix("hit"))
        .and_then(|digits| digits.parse::<u8>().ok())
        .filter(|hits| *hits > 0)
}

fn known_max_hp(belief: &UnknownBattleState, slot: FieldSlot) -> u16 {
    let mons = match slot.player {
        Player::P1 => &belief.p1_active_mons,
        Player::P2 => &belief.p2_active_mons,
    };
    // Own-side max HP is always exactly known (`min_stats[0] == max_stats[0]`);
    // for the opponent this is one of the very things inference is narrowing,
    // so masked opponent events never carry a trustworthy value here — 0 is the
    // same "unused filler" convention the hand-built inference test trees use
    // for percent-based opponent damage (see tracker_parse module docs).
    mons.get(slot.slot_index as usize)
        .map(|m| m.max_stats[0])
        .unwrap_or(0)
}

/// Build the `DamageDealt`/`Healed`/`SetHp` event for `slot` reaching `token`,
/// classified by comparing against the belief's current HP for that slot.
fn hp_event(
    belief: &UnknownBattleState,
    hp_readings: &mut HashMap<FieldSlot, PokemonHP>,
    slot: FieldSlot,
    token: HpToken,
) -> InformationEvent {
    let is_own = slot.player == Player::P1;
    let (new_hp, went_down, went_up) = match (hp_readings.get(&slot).cloned(), token) {
        (Some(PokemonHP::Percent(old)), HpToken::Percent(new)) => {
            (PokemonHP::Percent(new), new < old, new > old)
        }
        (Some(PokemonHP::Number(old)), HpToken::Number(new)) => {
            (PokemonHP::Number(new), new < old, new > old)
        }
        // Type mismatch (e.g. an exact number given for a slot the belief only
        // tracks as a percent) or no prior reading at all — best effort: honor
        // the literal token and assume it's damage (the overwhelmingly common
        // case for an untyped reading) rather than guessing a heal.
        (_, HpToken::Percent(new)) => (PokemonHP::Percent(new), true, false),
        (_, HpToken::Number(new)) => (PokemonHP::Number(new), true, false),
    };
    hp_readings.insert(slot, new_hp.clone());
    let max_hp = if is_own {
        known_max_hp(belief, slot)
    } else {
        0
    };
    let kind = if went_down {
        EventKind::DamageDealt {
            target: slot,
            new_hp,
            max_hp,
        }
    } else if went_up {
        EventKind::Healed {
            target: slot,
            new_hp,
            max_hp,
        }
    } else {
        EventKind::SetHp {
            target: slot,
            new_hp,
            max_hp,
        }
    };
    InformationEvent {
        kind,
        reactions: Vec::new(),
    }
}

fn typed_hp_event(
    belief: &UnknownBattleState,
    hp_readings: &mut HashMap<FieldSlot, PokemonHP>,
    slot: FieldSlot,
    token: HpToken,
    kind_word: &str,
) -> InformationEvent {
    let new_hp = match token {
        HpToken::Percent(value) => PokemonHP::Percent(value),
        HpToken::Number(value) => PokemonHP::Number(value),
    };
    hp_readings.insert(slot, new_hp.clone());
    let max_hp = if slot.player == Player::P1 {
        known_max_hp(belief, slot)
    } else {
        0
    };
    let kind = match kind_word {
        "damage" | "damaged" => EventKind::DamageDealt {
            target: slot,
            new_hp,
            max_hp,
        },
        "heal" | "healed" => EventKind::Healed {
            target: slot,
            new_hp,
            max_hp,
        },
        _ => EventKind::SetHp {
            target: slot,
            new_hp,
            max_hp,
        },
    };
    leaf(kind)
}

// ── stat boosts ──────────────────────────────────────────────────────────────

fn stat_idx(name: &str) -> Option<usize> {
    match name {
        "atk" | "attack" => Some(0),
        "def" | "defense" | "defence" => Some(1),
        "spa" | "spatk" | "spattack" | "specialattack" => Some(2),
        "spd" | "spdef" | "spdefense" | "specialdefense" => Some(3),
        "spe" | "speed" => Some(4),
        "acc" | "accuracy" => Some(5),
        "eva" | "evasion" | "evasiveness" => Some(6),
        _ => None,
    }
}

/// `atk+1` / `+1atk` / `atk-2` / `-2spe` — stat name and signed delta in either order.
/// Lowercases and drops everything but letters/digits/sign (unlike `norm`,
/// which would strip the `+`/`-` the sign parsing below depends on).
fn parse_boost_token(tok: &str) -> Option<(usize, i8)> {
    let lower: String = tok
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '+' || *c == '-')
        .map(|c| c.to_ascii_lowercase())
        .collect();
    // stat-name-first: split at the sign.
    if let Some(pos) = lower.find(['+', '-']) {
        let (name, signed) = lower.split_at(pos);
        if !name.is_empty()
            && let Some(idx) = stat_idx(name)
            && let Ok(n) = signed.parse::<i8>()
        {
            return Some((idx, n));
        }
        // sign-first: the sign is at position 0, name follows the digits.
        if pos == 0 {
            let sign: i8 = if signed.starts_with('-') { -1 } else { 1 };
            let rest = &signed[1..];
            let digit_end = rest
                .find(|c: char| !c.is_ascii_digit())
                .unwrap_or(rest.len());
            if digit_end > 0
                && let Ok(n) = rest[..digit_end].parse::<i8>()
                && let Some(idx) = stat_idx(&rest[digit_end..])
            {
                return Some((idx, sign * n));
            }
        }
    }
    None
}

// ── status / volatile / field words ─────────────────────────────────────────

fn status_from_word(word: &str) -> Option<Status> {
    match word {
        "brn" | "burn" | "burned" => Some(Status::Burn),
        "psn" | "poison" | "poisoned" => Some(Status::Poison),
        "tox" | "badpoison" | "badlypoisoned" | "toxic" => Some(Status::ToxicPoison(0)),
        "par" | "para" | "paralyzed" | "paralysis" | "paralysed" => Some(Status::Paralysis),
        "slp" | "sleep" | "asleep" => Some(Status::Sleep(0)),
        "frz" | "frozen" | "freeze" => Some(Status::Frozen(0)),
        _ => None,
    }
}

/// Reserved words that describe *why a Pokemon couldn't act*, used both as a
/// standalone `[slot] [failspec]` line and (for the same words) `Cant` events.
fn cant_reason_from_word(word: &str) -> Option<CantReason> {
    match word {
        "flinch" | "flinched" => Some(CantReason::Flinch),
        "fullpara" | "fullyparalyzed" | "fullparalysis" | "fullyparalysed" => {
            Some(CantReason::Paralysis)
        }
        "sleep" | "asleep" | "slp" => Some(CantReason::Sleep),
        "frozen" | "frz" | "freeze" => Some(CantReason::Freeze),
        "recharge" | "mustrecharge" | "recharging" => Some(CantReason::Recharge),
        "taunt" | "taunted" => Some(CantReason::Taunt),
        "disable" | "disabled" => Some(CantReason::Disable),
        "confusion" | "confused" => Some(CantReason::Confusion),
        "imprison" | "imprisoned" => Some(CantReason::Imprison),
        "attract" | "infatuated" | "infatuation" => Some(CantReason::Infatuation),
        "bound" | "trapped" => Some(CantReason::Bound),
        "throatchop" | "throatchopped" => Some(CantReason::ThroatChop),
        "torment" | "tormented" => Some(CantReason::Torment),
        "focuspunch" => Some(CantReason::FocusPunch),
        "gravity" => Some(CantReason::Gravity),
        "healblock" => Some(CantReason::HealBlock),
        "encore" | "encored" => Some(CantReason::Encore),
        "skydrop" => Some(CantReason::SkyDrop),
        "beakblast" => Some(CantReason::BeakBlast),
        _ => None,
    }
}

/// A representative subset of volatiles reachable by bare word (no payload).
/// Volatiles that carry a required payload (`Disable(move)`, `LockedMove(move)`,
/// `ChoiceLock(move)`, `Substitute(hp)`, …) aren't expressible this way yet —
/// documented gap, not silently wrong: `parse_line` rejects an unrecognized word
/// with a clear error rather than guessing.
fn volatile_from_word(word: &str) -> Option<VolatileStatus> {
    match word {
        "confusion" | "confused" => Some(VolatileStatus::Confusion),
        "leechseed" | "seeded" => Some(VolatileStatus::LeechSeed),
        "taunt" | "taunted" => Some(VolatileStatus::Taunt),
        "flashfire" => Some(VolatileStatus::FlashFire),
        "focusenergy" => Some(VolatileStatus::FocusEnergy),
        "aquaring" => Some(VolatileStatus::AquaRing),
        "attract" | "infatuated" => Some(VolatileStatus::Attract),
        "curse" | "cursed" => Some(VolatileStatus::Curse),
        "torment" | "tormented" => Some(VolatileStatus::Torment),
        "yawn" => Some(VolatileStatus::Yawn),
        "saltcure" => Some(VolatileStatus::SaltCure),
        "tarshot" => Some(VolatileStatus::TarShot),
        "minimize" | "minimized" => Some(VolatileStatus::Minimize),
        "ingrain" => Some(VolatileStatus::Ingrain),
        "magnetrise" => Some(VolatileStatus::MagnetRise),
        "protect" | "protected" => Some(VolatileStatus::Protect),
        "endure" | "enduring" => Some(VolatileStatus::Endure),
        "kingsshield" => Some(VolatileStatus::KingsShield),
        "banefulbunker" => Some(VolatileStatus::BanefulBunker),
        "spikyshield" => Some(VolatileStatus::SpikyShield),
        "silktrap" => Some(VolatileStatus::SilkTrap),
        "obstruct" => Some(VolatileStatus::Obstruct),
        "burningbulwark" => Some(VolatileStatus::BurningBulwark),
        "destinybond" => Some(VolatileStatus::DestinyBond),
        "grudge" => Some(VolatileStatus::Grudge),
        "embargo" => Some(VolatileStatus::Embargo),
        "healblock" => Some(VolatileStatus::HealBlock),
        "imprison" => Some(VolatileStatus::Imprison),
        "electrify" => Some(VolatileStatus::Electrify),
        "powder" => Some(VolatileStatus::Powder),
        "syrupbomb" => Some(VolatileStatus::SyrupBomb),
        "telekinesis" => Some(VolatileStatus::Telekinesis),
        "smackdown" => Some(VolatileStatus::SmackDown),
        "uproar" => Some(VolatileStatus::Uproar),
        "roost" => Some(VolatileStatus::Roost),
        "rage" => Some(VolatileStatus::Rage),
        "ragepowder" => Some(VolatileStatus::RagePowder),
        "followme" => Some(VolatileStatus::FollowMe),
        "magiccoat" => Some(VolatileStatus::MagicCoat),
        "snatch" => Some(VolatileStatus::Snatch),
        "laserfocus" => Some(VolatileStatus::LaserFocus),
        "miracleeye" => Some(VolatileStatus::MiracleEye),
        "foresight" => Some(VolatileStatus::Foresight),
        "octolock" => Some(VolatileStatus::OctoLock),
        "noretreat" => Some(VolatileStatus::NoRetreat),
        "gastroacid" => Some(VolatileStatus::GastroAcid),
        "sparklingaria" => Some(VolatileStatus::SparklingAria),
        "glaiverush" => Some(VolatileStatus::GlaiveRush),
        "charge" | "charged" => Some(VolatileStatus::Charge),
        "defensecurl" | "defensecurled" => Some(VolatileStatus::DefenseCurl),
        "helpinghand" => Some(VolatileStatus::HelpingHand),
        "powertrick" => Some(VolatileStatus::PowerTrick),
        "forestscurse" => Some(VolatileStatus::ForestsCurse),
        "throatchop" | "throatchopped" => Some(VolatileStatus::ThroatChop),
        "mustrecharge" | "recharging" => Some(VolatileStatus::MustRecharge),
        "substitute" | "sub" => Some(VolatileStatus::Substitute(0)),
        "encore" | "encored" => Some(VolatileStatus::Encore(PokemonMove::Struggle)),
        "disable" | "disabled" => Some(VolatileStatus::Disable(PokemonMove::Struggle)),
        // Bide, Sky Drop, Spotlight, Trick-or-Treat, Nightmare, and Power Shift
        // are all `isNonstandard: "Past"`/`"Unobtainable"` in the current move
        // dex — not legal in Pokémon Champions, so a real tracker session can
        // never actually need these words; intentionally left unmapped rather
        // than building grammar for content that can't occur (see CLAUDE.md's
        // "implement newest-generation behaviour" rule).
        _ => None,
    }
}

fn weather_from_word(word: &str) -> Option<Weather> {
    match word {
        "rain" | "raindance" | "drizzle" => Some(Weather::Rain),
        "heavyrain" | "primordialsea" => Some(Weather::HeavyRain),
        "sand" | "sandstorm" => Some(Weather::Sandstorm),
        "snow" | "hail" => Some(Weather::Snow),
        "sun" | "sunnyday" | "sunny" | "drought" => Some(Weather::Sun),
        "extremesun" | "desolateland" | "harshsunlight" => Some(Weather::ExtremeSunlight),
        "strongwinds" | "deltastream" => Some(Weather::StrongWinds),
        _ => None,
    }
}

fn terrain_from_word(word: &str) -> Option<Terrain> {
    match word {
        "electric" | "electricterrain" => Some(Terrain::ElectricTerrain),
        "grassy" | "grassyterrain" => Some(Terrain::GrassyTerrain),
        "misty" | "mistyterrain" => Some(Terrain::MistyTerrain),
        "psychic" | "psychicterrain" => Some(Terrain::PsychicTerrain),
        _ => None,
    }
}

fn pseudo_weather_from_word(word: &str) -> Option<PseudoWeather> {
    match word {
        "fairylock" => Some(PseudoWeather::FairyLock),
        "gravity" => Some(PseudoWeather::Gravity),
        "iondeluge" => Some(PseudoWeather::IonDeluge),
        "magicdeluge" => Some(PseudoWeather::MagicDeluge),
        "mudsport" => Some(PseudoWeather::MudSport),
        "trickroom" => Some(PseudoWeather::TrickRoom),
        "watersport" => Some(PseudoWeather::WaterSport),
        "wonderroom" => Some(PseudoWeather::WonderRoom),
        _ => None,
    }
}

fn side_condition_from_word(word: &str) -> Option<SideCondition> {
    match word {
        "auroraveil" => Some(SideCondition::AuroraVeil),
        "reflect" => Some(SideCondition::Reflect),
        "craftyshield" => Some(SideCondition::CraftyShield),
        "lightscreen" => Some(SideCondition::LightScreen),
        "luckychant" => Some(SideCondition::LuckyChant),
        "matblock" => Some(SideCondition::MatBlock),
        "mist" => Some(SideCondition::Mist),
        "quickguard" => Some(SideCondition::QuickGuard),
        "safeguard" => Some(SideCondition::SafeGuard),
        "spikes0" => Some(SideCondition::Spikes(0)),
        "spikes" | "spikes1" => Some(SideCondition::Spikes(1)),
        "spikes2" => Some(SideCondition::Spikes(2)),
        "spikes3" => Some(SideCondition::Spikes(3)),
        "stealthrock" => Some(SideCondition::StealthRock),
        "stickyweb" => Some(SideCondition::StickyWeb(None)),
        "tailwind" => Some(SideCondition::TailWind),
        "toxicspikes0" => Some(SideCondition::ToxicSpikes(0)),
        "toxicspikes" | "toxicspikes1" => Some(SideCondition::ToxicSpikes(1)),
        "toxicspikes2" => Some(SideCondition::ToxicSpikes(2)),
        "wideguard" => Some(SideCondition::WideGuard),
        _ => None,
    }
}

/// Resolve an item mention, trying common competitive-community shorthand
/// first (`Item::from_str` only strips punctuation/case — it has no notion of
/// abbreviation, so "sitrus" alone doesn't match "Sitrus Berry" without this).
/// Not exhaustive — expand as more abbreviations come up; anything not listed
/// here still falls through to `Item::from_str` unchanged (a full or
/// already-normalizable name always works either way).
fn item_from_word(word: &str) -> Item {
    let n = norm(word);
    let expanded = match n.as_str() {
        "sitrus" => "sitrusberry",
        "lum" => "lumberry",
        "chesto" => "chestoberry",
        "lefties" | "levs" => "leftovers",
        "helmet" => "rockyhelmet",
        "lo" => "lifeorb",
        "scarf" => "choicescarf",
        "specs" => "choicespecs",
        "band" => "choiceband",
        "boots" => "heavydutyboots",
        "wp" => "weaknesspolicy",
        "av" => "assaultvest",
        "sash" => "focussash",
        _ => return Item::from_str(word),
    };
    Item::from_str(expanded)
}

/// How an item mention changes what's held — shared by the standalone
/// `[slot] loses/consumes/gains [item]` line and the same verbs recognized
/// inline on a move line (`p1 tackle o1 40% p1 consumes sitrus 65%`), so a
/// berry eaten mid-line is recorded as gone (`ItemLost{consumed:true}`) —
/// not misrepresented as still held (a bare `ItemRevealed`).
#[derive(Clone, Copy)]
enum ItemVerb {
    Lost,
    Consumed,
    Gained,
}

fn item_verb_from_word(word: &str) -> Option<ItemVerb> {
    match word {
        "loses" | "lost" | "knockedoff" => Some(ItemVerb::Lost),
        "consumes" | "consumed" | "ate" | "eats" | "used" => Some(ItemVerb::Consumed),
        "gains" | "gained" | "tricked" | "switcheroo" | "recycles" => Some(ItemVerb::Gained),
        _ => None,
    }
}

fn item_verb_event(slot: FieldSlot, verb: ItemVerb, item: Item) -> EventKind {
    match verb {
        ItemVerb::Lost => EventKind::ItemLost {
            slot,
            item,
            consumed: false,
        },
        ItemVerb::Consumed => EventKind::ItemLost {
            slot,
            item,
            consumed: true,
        },
        ItemVerb::Gained => EventKind::ItemGained { slot, item },
    }
}

fn parse_type_word(word: &str) -> Option<PokemonType> {
    // `dex_data::parse_type` expects Title Case; accept any casing here.
    let mut chars = word.chars();
    let titled = match chars.next() {
        Some(c) => c.to_ascii_uppercase().to_string() + &chars.as_str().to_ascii_lowercase(),
        None => return None,
    };
    poke_rust::state::dex_data::parse_type(&titled)
}

// ── Line dispatch ────────────────────────────────────────────────────────────

fn err(line: usize, message: impl Into<String>) -> ParseError {
    ParseError {
        line,
        message: message.into(),
    }
}

fn parse_line(
    tokens: &[&str],
    line_no: usize,
    belief: &UnknownBattleState,
    move_dex: &HashMap<PokemonMove, MoveData>,
    // Used by the `mega` line to enumerate a species' possible mega forms.
    pokemon_dex: &HashMap<Species, PokemonData>,
    hp_readings: &mut HashMap<FieldSlot, PokemonHP>,
    // Running slot->species scratch, updated by `leads`/`switch` as they
    // parse — see `parse_tracker_text`'s doc comment. Read by `mega`'s
    // auto-fill/suffix resolution so a same-turn `leads` is visible to it.
    slot_species: &mut HashMap<FieldSlot, Species>,
) -> Result<TrackerLine, ParseError> {
    if norm(tokens[0]) == "endofturn" || norm(tokens[0]) == "eot" {
        if tokens.len() == 1 {
            return Ok(TrackerLine::EndOfTurn);
        }
        return match parse_line(
            &tokens[1..],
            line_no,
            belief,
            move_dex,
            pokemon_dex,
            hp_readings,
            slot_species,
        )? {
            TrackerLine::Event(event) => Ok(TrackerLine::EndOfTurnReaction(event)),
            TrackerLine::EndOfTurn | TrackerLine::EndOfTurnReaction(_) => {
                Err(err(line_no, "invalid nested end-of-turn marker"))
            }
        };
    }
    // Standalone field-effect lines: `weather rain`, `terrain electric`.
    match norm(tokens[0]).as_str() {
        "weather" => {
            let word = tokens
                .get(1)
                .ok_or_else(|| err(line_no, "weather requires a name"))?;
            let n = norm(word);
            let weather = if n == "none" || n == "clear" {
                None
            } else {
                Some(
                    weather_from_word(&n)
                        .ok_or_else(|| err(line_no, format!("unrecognized weather '{word}'")))?,
                )
            };
            return Ok(TrackerLine::Event(InformationEvent {
                kind: EventKind::WeatherChanged { weather },
                reactions: Vec::new(),
            }));
        }
        "terrain" => {
            let word = tokens
                .get(1)
                .ok_or_else(|| err(line_no, "terrain requires a name"))?;
            let n = norm(word);
            let terrain = if n == "none" || n == "clear" {
                None
            } else {
                Some(
                    terrain_from_word(&n)
                        .ok_or_else(|| err(line_no, format!("unrecognized terrain '{word}'")))?,
                )
            };
            return Ok(TrackerLine::Event(InformationEvent {
                kind: EventKind::TerrainChanged { terrain },
                reactions: Vec::new(),
            }));
        }
        "field" | "pseudoweather" => {
            let effect_tok = tokens
                .get(1)
                .ok_or_else(|| err(line_no, "field requires an effect name"))?;
            let effect = pseudo_weather_from_word(&norm(effect_tok))
                .ok_or_else(|| err(line_no, format!("unrecognized field effect '{effect_tok}'")))?;
            let state_tok = tokens
                .get(2)
                .ok_or_else(|| err(line_no, "field requires 'start' or 'end'"))?;
            let kind = match norm(state_tok).as_str() {
                "start" | "started" | "on" => EventKind::PseudoWeatherStart { effect },
                "end" | "ended" | "off" => EventKind::PseudoWeatherEnd { effect },
                _ => return Err(err(line_no, "field requires 'start' or 'end'")),
            };
            return Ok(TrackerLine::Event(InformationEvent {
                kind,
                reactions: Vec::new(),
            }));
        }
        // ── leads ────────────────────────────────────────────────────────
        // `leads [p|o] <species>...` — one side's battle-start (or
        // simultaneous post-faint replacement) leads, sent out together;
        // repeat the side marker to cover both sides on one line:
        // `leads p tyranitar lycanroc o charizard aerodactyl`. Species are
        // assigned left-to-right to that side's slots 0, 1, ... starting
        // from whichever side marker most recently appeared. Emits ONE
        // `SimultaneousSwitch` covering every named mon (both sides if both
        // appear); `fold_leads_and_entry_abilities` still folds a
        // following entry-ability reveal (`p1 sandstream`) into it.
        "leads" => {
            let rest = &tokens[1..];
            if rest.is_empty() {
                return Err(err(
                    line_no,
                    "leads requires a side ('p' or 'o') and at least one species",
                ));
            }
            let mut switches: Vec<SwitchState> = Vec::new();
            let mut current_side: Option<Player> = None;
            let mut next_slot_index: u8 = 0;
            for tok in rest {
                if let Some(side) = leads_side_marker(tok) {
                    current_side = Some(side);
                    next_slot_index = 0;
                    continue;
                }
                let Some(player) = current_side else {
                    return Err(err(
                        line_no,
                        format!("leads requires a side ('p' or 'o') before species — got '{tok}'"),
                    ));
                };
                let species = Species::from_str(tok);
                if matches!(species, Species::Unknown(_)) {
                    return Err(err(line_no, format!("unrecognized species '{tok}'")));
                }
                let lead_slot = FieldSlot {
                    player,
                    slot_index: next_slot_index,
                };
                let switch = build_switch_state(belief, lead_slot, species.clone());
                hp_readings.insert(lead_slot, switch.hp.clone());
                slot_species.insert(lead_slot, species);
                switches.push(switch);
                next_slot_index += 1;
            }
            if switches.is_empty() {
                return Err(err(line_no, "leads requires at least one species"));
            }
            return Ok(TrackerLine::Event(InformationEvent {
                kind: EventKind::SimultaneousSwitch { switches },
                reactions: Vec::new(),
            }));
        }
        "side" => {
            let side_tok = tokens
                .get(1)
                .ok_or_else(|| err(line_no, "side requires 'p' or 'o'"))?;
            let side = match norm(side_tok).as_str() {
                "p" | "p1" | "player" => Player::P1,
                "o" | "o1" | "opponent" => Player::P2,
                _ => return Err(err(line_no, "side requires 'p' or 'o'")),
            };
            let condition_tok = tokens
                .get(2)
                .ok_or_else(|| err(line_no, "side requires a condition name"))?;
            let condition = side_condition_from_word(&norm(condition_tok)).ok_or_else(|| {
                err(
                    line_no,
                    format!("unrecognized side condition '{condition_tok}'"),
                )
            })?;
            let state_tok = tokens
                .get(3)
                .ok_or_else(|| err(line_no, "side requires 'start' or 'end'"))?;
            let kind = match norm(state_tok).as_str() {
                "start" | "started" | "on" => EventKind::SideConditionStart { side, condition },
                "end" | "ended" | "off" => EventKind::SideConditionEnd { side, condition },
                _ => return Err(err(line_no, "side requires 'start' or 'end'")),
            };
            return Ok(TrackerLine::Event(InformationEvent {
                kind,
                reactions: Vec::new(),
            }));
        }
        _ => {}
    }

    let slot = parse_slot(tokens[0]).ok_or_else(|| {
        err(
            line_no,
            format!("expected a slot (p1/o1/…), got '{}'", tokens[0]),
        )
    })?;
    let action = tokens
        .get(1)
        .ok_or_else(|| err(line_no, "expected an action after the slot"))?;
    let action_n = norm(action);

    // ── switch ───────────────────────────────────────────────────────────
    // A single mid-battle replacement for one slot. For a whole side's
    // opening (or post-faint replacement) leads sent out together, use
    // `leads` instead — see its handler above, in the standalone-keyword
    // dispatch (its first token is the keyword `leads`, not a slot).
    if action_n == "switch" || action_n == "switchin" || action_n == "sendout" {
        let species_tok = tokens
            .get(2)
            .ok_or_else(|| err(line_no, "switch requires a species"))?;
        let species = Species::from_str(species_tok);
        if matches!(species, Species::Unknown(_)) {
            return Err(err(
                line_no,
                format!("unrecognized species '{species_tok}'"),
            ));
        }
        let mut switch = build_switch_state(belief, slot, species.clone());
        // Optional HP and major-status observations may follow in either order.
        // This matters for a previously-damaged/statused Pokémon returning from
        // the bench; `leads` are always fresh and need neither payload.
        for tok in &tokens[3..] {
            if let Some(hp_tok) = parse_hp_token(tok) {
                switch.hp = match hp_tok {
                    HpToken::Percent(p) => PokemonHP::Percent(p),
                    HpToken::Number(n) => PokemonHP::Number(n),
                };
            } else if let Some(status) = status_from_word(&norm(tok)) {
                switch.status = Some(status);
            } else {
                return Err(err(
                    line_no,
                    format!("unrecognized switch observation '{tok}'"),
                ));
            }
        }
        hp_readings.insert(slot, switch.hp.clone());
        slot_species.insert(slot, species);
        return Ok(TrackerLine::Event(InformationEvent {
            kind: EventKind::Switch(switch),
            reactions: Vec::new(),
        }));
    }

    // ── mega ─────────────────────────────────────────────────────────────
    // Three forms, checked in order: no token (auto-fill iff the slot's
    // current active species has exactly one mega form); a full species
    // token (unchanged); or a short suffix disambiguating a multi-mega
    // species (`o1 mega y` for Charizard-Mega-Y) when the token doesn't
    // parse as a real species on its own.
    if action_n == "mega" || action_n == "megaevolve" || action_n == "megaevolution" {
        let into = match tokens.get(2) {
            None => {
                let species = active_species_at(belief, slot_species, slot).ok_or_else(|| {
                    err(
                        line_no,
                        "mega requires a species — this slot's species isn't known yet",
                    )
                })?;
                let forms = poke_rust::state::pokemon::mega_forms_of(&species, pokemon_dex);
                match forms.as_slice() {
                    [only] => only.clone(),
                    [] => {
                        return Err(err(
                            line_no,
                            format!("{species:?} has no known mega form — specify one"),
                        ));
                    }
                    _ => {
                        return Err(err(
                            line_no,
                            format!(
                                "{species:?} has multiple mega forms — specify which (e.g. 'mega y')"
                            ),
                        ));
                    }
                }
            }
            Some(species_tok) => {
                let candidate = Species::from_str(species_tok);
                if !matches!(candidate, Species::Unknown(_)) {
                    candidate
                } else {
                    let species = active_species_at(belief, slot_species, slot).ok_or_else(|| {
                        err(line_no, format!("unrecognized species '{species_tok}'"))
                    })?;
                    let forms = poke_rust::state::pokemon::mega_forms_of(&species, pokemon_dex);
                    let suffix = norm(species_tok);
                    let matches: Vec<Species> = forms
                        .into_iter()
                        .filter(|f| norm(&f.to_string()).ends_with(&suffix))
                        .collect();
                    match matches.as_slice() {
                        [only] => only.clone(),
                        _ => {
                            return Err(err(
                                line_no,
                                format!(
                                    "unrecognized species or ambiguous mega suffix '{species_tok}'"
                                ),
                            ));
                        }
                    }
                }
            }
        };
        return Ok(TrackerLine::Event(InformationEvent {
            kind: EventKind::MegaEvolution { slot, into },
            reactions: Vec::new(),
        }));
    }

    // ── tera ─────────────────────────────────────────────────────────────
    if action_n == "tera" || action_n == "terastallize" || action_n == "terastallized" {
        let type_tok = tokens
            .get(2)
            .ok_or_else(|| err(line_no, "tera requires a type"))?;
        let tera_type = parse_type_word(type_tok)
            .ok_or_else(|| err(line_no, format!("unrecognized type '{type_tok}'")))?;
        return Ok(TrackerLine::Event(InformationEvent {
            kind: EventKind::Terastallization { slot, tera_type },
            reactions: Vec::new(),
        }));
    }

    if action_n == "mustrecharge" {
        return Ok(TrackerLine::Event(InformationEvent {
            kind: EventKind::MustRecharge { slot },
            reactions: Vec::new(),
        }));
    }

    // `[slot] charging <move>` — an INPUT ALIAS, kept because it reads naturally and
    // predates the canonical form. It desugars to the same tree the canonical
    // `[slot] <move> charging` produces (see `parse_move_line`'s `charging` arm): a
    // `MoveUsed` wrapping a `ChargingMove` reaction. That shape is not cosmetic — it
    // is exactly what the engine emits, because `execute_action` folds everything a
    // move emitted into `MoveUsed.reactions`, so a top-level bare `ChargingMove` can
    // never occur in a battle-mode log. Keeping one canonical tree is what lets
    // `tracker_render.rs` render a charge turn back to text at all (a top-level
    // `ChargingMove` has no renderer) and lets tracker text round-trip with a real
    // battle log.
    //
    // `targets` is deliberately empty: on a charge turn you usually cannot tell what
    // the opponent aimed at — "Charizard flew up high!" names no target — and the
    // release turn reveals it. Inference is fine with that; the structural pass
    // ignores `MoveUsed`'s targets and the damage->stat-bounds pass only engages when
    // there is damage, which a charge turn has none of.
    if action_n == "charging" {
        let move_tok = tokens
            .get(2)
            .ok_or_else(|| err(line_no, "charging requires a move name"))?;
        let move_used = PokemonMove::from_str(move_tok);
        if !move_dex.contains_key(&move_used) {
            return Err(err(line_no, format!("unrecognized move '{move_tok}'")));
        }
        return Ok(TrackerLine::Event(InformationEvent {
            kind: EventKind::MoveUsed {
                user: slot,
                move_used: move_used.clone(),
                targets: Vec::new(),
            },
            reactions: vec![leaf(EventKind::ChargingMove {
                user: slot,
                move_used,
            })],
        }));
    }

    if action_n == "illusion" || action_n == "illusionended" {
        let species_tok = tokens
            .get(2)
            .ok_or_else(|| err(line_no, "illusion requires the actual species"))?;
        let actual_species = Species::from_str(species_tok);
        if matches!(actual_species, Species::Unknown(_)) {
            return Err(err(
                line_no,
                format!("unrecognized species '{species_tok}'"),
            ));
        }
        return Ok(TrackerLine::Event(InformationEvent {
            kind: EventKind::IllusionEnded {
                slot,
                actual_species,
            },
            reactions: Vec::new(),
        }));
    }

    if action_n == "encoremove" || action_n == "disablemove" {
        let move_tok = tokens
            .get(2)
            .ok_or_else(|| err(line_no, format!("{action} requires a move name")))?;
        let payload = PokemonMove::from_str(move_tok);
        if matches!(payload, PokemonMove::Unknown(_)) {
            return Err(err(line_no, format!("unrecognized move '{move_tok}'")));
        }
        let volatile = if action_n == "encoremove" {
            VolatileStatus::Encore(payload)
        } else {
            VolatileStatus::Disable(payload)
        };
        return Ok(TrackerLine::Event(leaf(EventKind::VolatileStart {
            target: slot,
            volatile,
        })));
    }
    if action_n == "stockpilelevel" {
        let level = tokens
            .get(2)
            .and_then(|token| token.parse::<u8>().ok())
            .filter(|level| (1..=3).contains(level))
            .ok_or_else(|| err(line_no, "stockpilelevel requires 1, 2, or 3"))?;
        return Ok(TrackerLine::Event(leaf(EventKind::VolatileStart {
            target: slot,
            volatile: VolatileStatus::Stockpile(level),
        })));
    }

    // Explicit no-action marker for an empty/fainted slot. The simulator has
    // no dedicated Pass information event, so use the generic Cant reason:
    // inference treats it as an action-slot commitment with no extra claim.
    if action_n == "pass" {
        return Ok(TrackerLine::Event(InformationEvent {
            kind: EventKind::Cant {
                slot,
                reason: CantReason::Other,
            },
            reactions: Vec::new(),
        }));
    }

    // Standalone HP observation for residual/end-of-turn changes that have no
    // enclosing move line: `p1 hp 88hp` / `o1 hp 72%`.
    if action_n == "hp" {
        let hp_tok = tokens
            .get(2)
            .and_then(|token| parse_hp_token(token))
            .ok_or_else(|| err(line_no, "hp requires a value such as 88hp or 72%"))?;
        return Ok(TrackerLine::Event(hp_event(
            belief,
            hp_readings,
            slot,
            hp_tok,
        )));
    }

    if matches!(
        action_n.as_str(),
        "damage" | "damaged" | "heal" | "healed" | "sethp"
    ) {
        let hp_tok = tokens
            .get(2)
            .and_then(|token| parse_hp_token(token))
            .ok_or_else(|| {
                err(
                    line_no,
                    format!("{action} requires a value such as 88hp or 72%"),
                )
            })?;
        return Ok(TrackerLine::Event(typed_hp_event(
            belief,
            hp_readings,
            slot,
            hp_tok,
            &action_n,
        )));
    }

    if action_n == "volatileend" || action_n == "endvolatile" {
        let volatile_tok = tokens
            .get(2)
            .ok_or_else(|| err(line_no, "volatileend requires a volatile name"))?;
        let volatile = volatile_from_word(&norm(volatile_tok)).ok_or_else(|| {
            err(
                line_no,
                format!("unrecognized payload-free volatile '{volatile_tok}'"),
            )
        })?;
        return Ok(TrackerLine::Event(InformationEvent {
            kind: EventKind::VolatileEnd {
                target: slot,
                volatile,
            },
            reactions: Vec::new(),
        }));
    }

    if action_n == "cure" || action_n == "statuscured" {
        let status_tok = tokens
            .get(2)
            .ok_or_else(|| err(line_no, "cure requires a status name"))?;
        let status = status_from_word(&norm(status_tok))
            .ok_or_else(|| err(line_no, format!("unrecognized status '{status_tok}'")))?;
        return Ok(TrackerLine::Event(InformationEvent {
            kind: EventKind::StatusCured {
                target: slot,
                status,
            },
            reactions: Vec::new(),
        }));
    }
    if action_n == "status" || action_n == "statusinflicted" {
        let status_tok = tokens
            .get(2)
            .ok_or_else(|| err(line_no, "status requires a status name"))?;
        let status = status_from_word(&norm(status_tok))
            .ok_or_else(|| err(line_no, format!("unrecognized status '{status_tok}'")))?;
        return Ok(TrackerLine::Event(leaf(EventKind::StatusInflicted {
            target: slot,
            status,
        })));
    }

    // `p1 copyboosts o1`: p1 copied o1's complete boost table (Psych Up).
    if action_n == "copyboosts" || action_n == "boostscopied" {
        let source_tok = tokens
            .get(2)
            .ok_or_else(|| err(line_no, "copyboosts requires a source slot"))?;
        let source = parse_slot(source_tok)
            .ok_or_else(|| err(line_no, format!("invalid source slot '{source_tok}'")))?;
        return Ok(TrackerLine::Event(InformationEvent {
            kind: EventKind::BoostsCopied {
                source,
                target: slot,
            },
            reactions: Vec::new(),
        }));
    }
    if action_n == "invertboosts" || action_n == "boostsinverted" {
        return Ok(TrackerLine::Event(leaf(EventKind::BoostsInverted {
            target: slot,
        })));
    }

    // Standalone stat change, primarily for end-of-turn ability effects such
    // as Speed Boost: `p1 spe+1`.
    if let Some((boost_idx, stages)) = parse_boost_token(action) {
        return Ok(TrackerLine::Event(InformationEvent {
            kind: EventKind::BoostChanged {
                target: slot,
                boost_idx,
                stages,
            },
            reactions: Vec::new(),
        }));
    }

    // ── item verbs: `p1 loses leftovers`, `p1 gains choicescarf`, bare `p1 leftovers` ──
    if let Some(verb) = item_verb_from_word(&action_n) {
        let item_tok = tokens
            .get(2)
            .ok_or_else(|| err(line_no, "expected an item name"))?;
        let item = item_from_word(item_tok);
        if !matches!(item, Item::Unknown(_)) {
            return Ok(TrackerLine::Event(InformationEvent {
                kind: item_verb_event(slot, verb, item),
                reactions: Vec::new(),
            }));
        }
        // Verb matched but the following word isn't a real item — fall through
        // to the ordinary single-token dispatch below (it wasn't an item line).
    }

    // ── failspec (no move context) ──────────────────────────────────────
    // Bare TWO-token lines only (`[slot] [reason]`, matching
    // `cant_reason_from_word`'s own documented convention). Several
    // cant-reason words are ALSO real move names (Taunt, Disable, Encore,
    // Confusion, Attract, Throat Chop, Torment, Focus Punch, Gravity, Heal
    // Block) — a longer line (target/effect tokens follow) means the word is
    // being used AS that move, not as a cant-reason; falling through to the
    // move dispatch below preserves those extra tokens instead of silently
    // discarding them (the bug this guard fixes: `p1 taunt o1` used to parse
    // as `Cant{Taunt}`, dropping the `o1` target and the move usage entirely).
    if tokens.len() == 2
        && let Some(reason) = cant_reason_from_word(&action_n)
    {
        return Ok(TrackerLine::Event(InformationEvent {
            kind: EventKind::Cant { slot, reason },
            reactions: Vec::new(),
        }));
    }

    // ── move ─────────────────────────────────────────────────────────────
    let candidate_move = PokemonMove::from_str(action);
    if move_dex.contains_key(&candidate_move) {
        return parse_move_line(
            slot,
            candidate_move,
            &tokens[2..],
            line_no,
            belief,
            hp_readings,
            move_dex,
        );
    }

    // ── ability reveal ──────────────────────────────────────────────────
    let candidate_ability = Ability::from_str(action);
    if !matches!(candidate_ability, Ability::Unknown(_) | Ability::None) {
        let ability_event = InformationEvent {
            kind: EventKind::AbilityRevealed {
                slot,
                ability: candidate_ability,
            },
            reactions: Vec::new(),
        };
        return Ok(TrackerLine::Event(ability_event));
    }

    // ── bare item reveal (no verb) ───────────────────────────────────────
    let candidate_item = item_from_word(action);
    if !matches!(candidate_item, Item::Unknown(_) | Item::None) {
        return Ok(TrackerLine::Event(InformationEvent {
            kind: EventKind::ItemRevealed {
                slot,
                item: candidate_item,
            },
            reactions: Vec::new(),
        }));
    }

    Err(err(
        line_no,
        format!("'{action}' is not a recognized move, ability, item, or keyword"),
    ))
}

fn full_hp_for(slot: FieldSlot, max_hp: u16) -> PokemonHP {
    if slot.player == Player::P1 {
        PokemonHP::Number(max_hp)
    } else {
        PokemonHP::Percent(100)
    }
}

/// The species currently occupying `slot`, when it's already known — either
/// because an earlier line THIS SAME SUBMISSION already sent it there
/// (`slot_species`, checked first — see `parse_tracker_text`'s doc comment
/// for why the frozen `belief` alone isn't enough) or because it was already
/// `Known` in the belief carried over from a prior turn (an opponent's
/// active is only `Known` once revealed by an earlier switch/reveal; the
/// viewer's own side is always `Known`). Used by the `mega` line to look up
/// which mega forms are even possible.
fn active_species_at(
    belief: &UnknownBattleState,
    slot_species: &HashMap<FieldSlot, Species>,
    slot: FieldSlot,
) -> Option<Species> {
    if let Some(species) = slot_species.get(&slot) {
        return Some(species.clone());
    }
    let mons = match slot.player {
        Player::P1 => &belief.p1_active_mons,
        Player::P2 => &belief.p2_active_mons,
    };
    match &mons.get(slot.slot_index as usize)?.possible_species {
        poke_rust::information::unknowns::Unknown::Known(s) => Some(s.clone()),
        _ => None,
    }
}

/// Find `species` wherever it currently sits in `player`'s roster (active,
/// known-back, possible-back, or fainted) — every roster member the viewer's
/// own side seeds is fully `Known` from turn 0 (their real teamsheet, seeded
/// at tracker creation), so a bench member re-entering already has an exact
/// entry recorded somewhere in the belief; an opponent species is `Known`
/// once revealed (team-preview species reveal, or an earlier switch/reveal),
/// even while everything else about it stays fogged.
fn find_roster_mon<'a>(
    belief: &'a UnknownBattleState,
    player: Player,
    species: &Species,
) -> Option<&'a poke_rust::information::unknowns::UnknownPokemonState> {
    let buckets = match player {
        Player::P1 => [
            &belief.p1_active_mons,
            &belief.p1_known_back_mons,
            &belief.p1_possible_back_mons,
            &belief.p1_fainted_mons,
        ],
        Player::P2 => [
            &belief.p2_active_mons,
            &belief.p2_known_back_mons,
            &belief.p2_possible_back_mons,
            &belief.p2_fainted_mons,
        ],
    };
    buckets.into_iter().find_map(|bucket| {
        bucket.iter().find(|m| {
            matches!(&m.possible_species, poke_rust::information::unknowns::Unknown::Known(s) if s == species)
        })
    })
}

/// Build the `SwitchState` for `species` entering `slot` — shared by the
/// single-slot `switch` line and the whole-side `leads` line. For the
/// viewer's own side, pulls the real level/status/exact-HP from wherever that
/// species already sits in the belief (see `find_roster_mon`'s doc comment);
/// for the opponent, assumes a fresh 100%-HP send-out with no status (the
/// caller — `switch`'s optional trailing hpspec — may override `hp`
/// afterward; `leads` never does, a lead is always fresh).
fn build_switch_state(
    belief: &UnknownBattleState,
    slot: FieldSlot,
    species: Species,
) -> SwitchState {
    if slot.player == Player::P1
        && let Some(mon) = find_roster_mon(belief, Player::P1, &species)
    {
        return SwitchState {
            slot,
            species,
            level: mon.level,
            hp: mon.hp.clone(),
            status: mon.status.clone(),
            tera_type: None,
            disguise_species: None,
            max_hp: mon.max_stats[0],
        };
    }
    let max_hp = find_roster_mon(belief, slot.player, &species)
        .map(|m| m.max_stats[0])
        .unwrap_or(0);
    SwitchState {
        slot,
        species,
        level: 50,
        hp: full_hp_for(slot, max_hp),
        status: None,
        tera_type: None,
        disguise_species: None,
        max_hp,
    }
}

/// A move line's trailing tokens: `[target effectspec...]*`. Every effect
/// attaches to whichever slot was most recently named — see the module-level
/// "flat nesting" simplification.
fn parse_move_line(
    user: FieldSlot,
    move_used: PokemonMove,
    rest: &[&str],
    line_no: usize,
    belief: &UnknownBattleState,
    hp_readings: &mut HashMap<FieldSlot, PokemonHP>,
    move_dex: &HashMap<PokemonMove, MoveData>,
) -> Result<TrackerLine, ParseError> {
    let mut targets: Vec<FieldSlot> = Vec::new();
    let mut children: Vec<InformationEvent> = Vec::new();
    let mut current = user;
    let mut has_explicit_targets = false;

    let mut i = 0;
    while i < rest.len() {
        let tok = rest[i];
        let n = norm(tok);

        // `@slot` is an unambiguous target declaration. Plain slot tokens
        // retain the original shorthand, where naming the user only changes
        // the attachment point for recoil/self effects. The explicit form is
        // required when an auto-target move's target set includes its user.
        if let Some(target_token) = tok.strip_prefix('@')
            && let Some(target) = parse_slot(target_token)
        {
            current = target;
            has_explicit_targets = true;
            if !targets.contains(&target) {
                targets.push(target);
            }
            i += 1;
            continue;
        }
        if let Some(s) = parse_slot(tok) {
            current = s;
            if !has_explicit_targets && !targets.contains(&s) && s != user {
                targets.push(s);
            }
            i += 1;
            continue;
        }
        if n == "crit" {
            children.push(leaf(EventKind::Crit { target: current }));
        } else if n == "miss" || n == "missed" {
            children.push(leaf(EventKind::Missed { target: current }));
        } else if n == "immune" {
            children.push(leaf(EventKind::Immune { target: current }));
        } else if n == "blocked" || n == "block" {
            children.push(leaf(EventKind::Blocked { target: current }));
        } else if n == "fail" || n == "failed" {
            children.push(leaf(EventKind::MoveFailed { slot: current }));
        } else if n == "mustrecharge" {
            children.push(leaf(EventKind::MustRecharge { slot: current }));
        } else if n == "charging" {
            // The move name is OPTIONAL here — `o1 solarbeam charging` is the
            // canonical form. A charge turn's `ChargingMove` always names the same
            // move as its enclosing `MoveUsed` (see `handle_charging_first_turn` in
            // `simulator/mod.rs`, which emits `action.move_name` verbatim), so
            // repeating it carries no information. The redundant spelling
            // `o1 solarbeam charging solarbeam` is still accepted, both for
            // backwards compatibility and because it's what a naive reading of the
            // grammar suggests.
            //
            // Only consume the next token when it names THIS line's move: anything
            // else there is a following effect token (`p2`, `crit`, …) that the main
            // loop must still see. A different real move name is a typo, not a
            // second charge, so it's rejected outright rather than silently ignored.
            if let Some(next) = rest.get(i + 1) {
                let next_move = PokemonMove::from_str(next);
                if next_move == move_used {
                    i += 1;
                } else if move_dex.contains_key(&next_move) {
                    return Err(err(
                        line_no,
                        format!(
                            "'charging {next}' does not match this line's move \
                             '{move_used:?}' — a charge turn always charges the move being used"
                        ),
                    ));
                }
            }
            children.push(leaf(EventKind::ChargingMove {
                user: current,
                move_used: move_used.clone(),
            }));
        } else if n == "illusion" || n == "illusionended" {
            i += 1;
            let species_tok = rest
                .get(i)
                .ok_or_else(|| err(line_no, format!("'{tok}' requires the actual species")))?;
            let actual_species = Species::from_str(species_tok);
            if matches!(actual_species, Species::Unknown(_)) {
                return Err(err(
                    line_no,
                    format!("unrecognized species '{species_tok}'"),
                ));
            }
            children.push(leaf(EventKind::IllusionEnded {
                slot: current,
                actual_species,
            }));
        } else if n == "switch" || n == "switchin" || n == "sendout" {
            i += 1;
            let species_tok = rest
                .get(i)
                .ok_or_else(|| err(line_no, format!("'{tok}' requires a species")))?;
            let species = Species::from_str(species_tok);
            if matches!(species, Species::Unknown(_)) {
                return Err(err(
                    line_no,
                    format!("unrecognized species '{species_tok}'"),
                ));
            }
            let mut switch = build_switch_state(belief, current, species);
            if let Some(next) = rest.get(i + 1)
                && let Some(hp_tok) = parse_hp_token(next)
            {
                switch.hp = match hp_tok {
                    HpToken::Percent(value) => PokemonHP::Percent(value),
                    HpToken::Number(value) => PokemonHP::Number(value),
                };
                i += 1;
            }
            if let Some(next) = rest.get(i + 1)
                && let Some(status) = status_from_word(&norm(next))
            {
                switch.status = Some(status);
                i += 1;
            }
            hp_readings.insert(current, switch.hp.clone());
            children.push(leaf(EventKind::Switch(switch)));
        } else if let Some(hits) = parse_hit_count_token(tok) {
            children.push(leaf(EventKind::HitCount {
                target: current,
                hits,
            }));
        } else if matches!(
            n.as_str(),
            "damage" | "damaged" | "heal" | "healed" | "sethp"
        ) {
            i += 1;
            let hp_tok = rest
                .get(i)
                .and_then(|token| parse_hp_token(token))
                .ok_or_else(|| err(line_no, format!("'{tok}' requires an HP value")))?;
            children.push(typed_hp_event(belief, hp_readings, current, hp_tok, &n));
        } else if let Some(hp) = parse_hp_token(tok) {
            children.push(hp_event(belief, hp_readings, current, hp));
        } else if let Some((idx, delta)) = parse_boost_token(tok) {
            children.push(leaf(EventKind::BoostChanged {
                target: current,
                boost_idx: idx,
                stages: delta,
            }));
        } else if let Some(status) = status_from_word(&n) {
            children.push(leaf(EventKind::StatusInflicted {
                target: current,
                status,
            }));
        } else if n == "cure" || n == "statuscured" {
            i += 1;
            let status_tok = rest
                .get(i)
                .ok_or_else(|| err(line_no, format!("'{tok}' requires a status name")))?;
            let status = status_from_word(&norm(status_tok))
                .ok_or_else(|| err(line_no, format!("unrecognized status '{status_tok}'")))?;
            children.push(leaf(EventKind::StatusCured {
                target: current,
                status,
            }));
        } else if n == "copyboosts" || n == "boostscopied" {
            i += 1;
            let source_tok = rest
                .get(i)
                .ok_or_else(|| err(line_no, format!("'{tok}' requires a source slot")))?;
            let source = parse_slot(source_tok)
                .ok_or_else(|| err(line_no, format!("invalid source slot '{source_tok}'")))?;
            children.push(leaf(EventKind::BoostsCopied {
                source,
                target: current,
            }));
        } else if n == "invertboosts" || n == "boostsinverted" {
            children.push(leaf(EventKind::BoostsInverted { target: current }));
        } else if n == "volatileend" || n == "endvolatile" {
            i += 1;
            let volatile_tok = rest
                .get(i)
                .ok_or_else(|| err(line_no, format!("'{tok}' requires a volatile name")))?;
            let volatile = volatile_from_word(&norm(volatile_tok))
                .ok_or_else(|| err(line_no, format!("unrecognized volatile '{volatile_tok}'")))?;
            children.push(leaf(EventKind::VolatileEnd {
                target: current,
                volatile,
            }));
        } else if n == "encoremove" || n == "disablemove" {
            i += 1;
            let move_tok = rest
                .get(i)
                .ok_or_else(|| err(line_no, format!("'{tok}' requires a move name")))?;
            let payload = PokemonMove::from_str(move_tok);
            if matches!(payload, PokemonMove::Unknown(_)) {
                return Err(err(line_no, format!("unrecognized move '{move_tok}'")));
            }
            let volatile = if n == "encoremove" {
                VolatileStatus::Encore(payload)
            } else {
                VolatileStatus::Disable(payload)
            };
            children.push(leaf(EventKind::VolatileStart {
                target: current,
                volatile,
            }));
        } else if n == "stockpilelevel" {
            i += 1;
            let level = rest
                .get(i)
                .and_then(|token| token.parse::<u8>().ok())
                .filter(|level| (1..=3).contains(level))
                .ok_or_else(|| err(line_no, "stockpilelevel requires 1, 2, or 3"))?;
            children.push(leaf(EventKind::VolatileStart {
                target: current,
                volatile: VolatileStatus::Stockpile(level),
            }));
        } else if n == "weather" {
            i += 1;
            let weather_tok = rest
                .get(i)
                .ok_or_else(|| err(line_no, "weather requires a name"))?;
            let normalized = norm(weather_tok);
            let weather = if normalized == "none" || normalized == "clear" {
                None
            } else {
                Some(weather_from_word(&normalized).ok_or_else(|| {
                    err(line_no, format!("unrecognized weather '{weather_tok}'"))
                })?)
            };
            children.push(leaf(EventKind::WeatherChanged { weather }));
        } else if n == "terrain" {
            i += 1;
            let terrain_tok = rest
                .get(i)
                .ok_or_else(|| err(line_no, "terrain requires a name"))?;
            let normalized = norm(terrain_tok);
            let terrain = if normalized == "none" || normalized == "clear" {
                None
            } else {
                Some(terrain_from_word(&normalized).ok_or_else(|| {
                    err(line_no, format!("unrecognized terrain '{terrain_tok}'"))
                })?)
            };
            children.push(leaf(EventKind::TerrainChanged { terrain }));
        } else if n == "field" || n == "pseudoweather" {
            let effect_tok = rest
                .get(i + 1)
                .ok_or_else(|| err(line_no, format!("'{tok}' requires an effect name")))?;
            let state_tok = rest
                .get(i + 2)
                .ok_or_else(|| err(line_no, format!("'{tok}' requires 'start' or 'end'")))?;
            let effect = pseudo_weather_from_word(&norm(effect_tok))
                .ok_or_else(|| err(line_no, format!("unrecognized field effect '{effect_tok}'")))?;
            let kind = match norm(state_tok).as_str() {
                "start" | "started" | "on" => EventKind::PseudoWeatherStart { effect },
                "end" | "ended" | "off" => EventKind::PseudoWeatherEnd { effect },
                _ => return Err(err(line_no, format!("'{tok}' requires 'start' or 'end'"))),
            };
            children.push(leaf(kind));
            i += 2;
        } else if n == "side" {
            let side_tok = rest
                .get(i + 1)
                .ok_or_else(|| err(line_no, "side requires 'p' or 'o'"))?;
            let side = match norm(side_tok).as_str() {
                "p" | "p1" | "player" => Player::P1,
                "o" | "o1" | "opponent" => Player::P2,
                _ => return Err(err(line_no, "side requires 'p' or 'o'")),
            };
            let condition_tok = rest
                .get(i + 2)
                .ok_or_else(|| err(line_no, "side requires a condition name"))?;
            let condition = side_condition_from_word(&norm(condition_tok)).ok_or_else(|| {
                err(
                    line_no,
                    format!("unrecognized side condition '{condition_tok}'"),
                )
            })?;
            let state_tok = rest
                .get(i + 3)
                .ok_or_else(|| err(line_no, "side requires 'start' or 'end'"))?;
            let kind = match norm(state_tok).as_str() {
                "start" | "started" | "on" => EventKind::SideConditionStart { side, condition },
                "end" | "ended" | "off" => EventKind::SideConditionEnd { side, condition },
                _ => return Err(err(line_no, "side requires 'start' or 'end'")),
            };
            children.push(leaf(kind));
            i += 3;
        } else if let Some(volatile) = volatile_from_word(&n) {
            children.push(leaf(EventKind::VolatileStart {
                target: current,
                volatile,
            }));
        } else if let Some(verb) = item_verb_from_word(&n) {
            // `p1 tackle o1 40% p1 consumes sitrus 65%` — same verbs as the
            // standalone item line, so a berry eaten mid-line is recorded as
            // gone (`ItemLost{consumed:true}`), not a bare `ItemRevealed`
            // that would leave it looking still-held.
            i += 1;
            let item_tok = rest
                .get(i)
                .ok_or_else(|| err(line_no, format!("'{tok}' requires an item name")))?;
            let item = item_from_word(item_tok);
            if matches!(item, Item::Unknown(_) | Item::None) {
                return Err(err(line_no, format!("unrecognized item '{item_tok}'")));
            }
            children.push(leaf(item_verb_event(current, verb, item)));
        } else {
            // Try ability / item reveal inline (e.g. "rockyHelmet", "roughskin").
            let ability = Ability::from_str(tok);
            if !matches!(ability, Ability::Unknown(_) | Ability::None) {
                children.push(leaf(EventKind::AbilityRevealed {
                    slot: current,
                    ability,
                }));
            } else {
                let item = item_from_word(tok);
                if !matches!(item, Item::Unknown(_) | Item::None) {
                    children.push(leaf(EventKind::ItemRevealed {
                        slot: current,
                        item,
                    }));
                } else {
                    return Err(err(
                        line_no,
                        format!("unrecognized target/effect token '{tok}' in move line"),
                    ));
                }
            }
        }
        i += 1;
    }

    Ok(TrackerLine::Event(InformationEvent {
        kind: EventKind::MoveUsed {
            user,
            move_used,
            targets,
        },
        reactions: children,
    }))
}

fn leaf(kind: EventKind) -> InformationEvent {
    InformationEvent {
        kind,
        reactions: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tracker_effects::augment_with_guaranteed_effects;
    use poke_rust::data::ability::Ability;
    use poke_rust::information::inference::{InferenceConfig, apply_information};
    use poke_rust::information::unknowns::UnknownMatchState;
    use poke_rust::state::dex_data::{parse_ability_dex, parse_move_dex, parse_pokemon_dex};
    use rand::rngs::StdRng;
    use rand::{Rng, SeedableRng};
    use std::sync::OnceLock;

    static POKEMON_DEX: OnceLock<HashMap<Species, PokemonData>> = OnceLock::new();
    static MOVE_DEX: OnceLock<HashMap<PokemonMove, MoveData>> = OnceLock::new();
    static ABILITY_DEX: OnceLock<HashMap<Ability, poke_rust::state::dex_data::AbilityData>> =
        OnceLock::new();

    fn pokemon_dex() -> &'static HashMap<Species, PokemonData> {
        POKEMON_DEX.get_or_init(|| parse_pokemon_dex("../pokemon_info/showdownDex.txt"))
    }
    fn move_dex() -> &'static HashMap<PokemonMove, MoveData> {
        MOVE_DEX.get_or_init(|| parse_move_dex("../pokemon_info/showdownMoves.txt"))
    }
    fn ability_dex() -> &'static HashMap<Ability, poke_rust::state::dex_data::AbilityData> {
        ABILITY_DEX.get_or_init(|| parse_ability_dex("../pokemon_info/showdownAbilities.txt"))
    }

    fn make_active(
        species: Species,
        hp: PokemonHP,
    ) -> poke_rust::information::unknowns::UnknownPokemonState {
        let mut mon = poke_rust::information::unknowns::UnknownPokemonState::from_opponent_species(
            species,
            pokemon_dex(),
            50,
        );
        mon.hp = hp;
        mon
    }

    /// A minimal 1v1 belief: P1's own Pikachu (fully known, exact HP) vs. an
    /// opponent Garchomp (species known — as if revealed by an earlier switch
    /// — everything else still fogged).
    fn test_belief() -> UnknownBattleState {
        test_belief_with(Species::Pikachu, Species::Garchomp)
    }

    /// Same shape as `test_belief`, with the active species swapped in —
    /// used by tests that need a specific (e.g. mega-capable) species active.
    fn test_belief_with(p1_species: Species, p2_species: Species) -> UnknownBattleState {
        UnknownBattleState {
            active_per_side: 1,
            back_mons_per_side: 0,
            p1_active_mons: vec![make_active(p1_species, PokemonHP::Number(100))],
            p2_active_mons: vec![make_active(p2_species, PokemonHP::Percent(100))],
            p1_known_back_mons: Vec::new(),
            p2_known_back_mons: Vec::new(),
            p1_possible_back_mons: Vec::new(),
            p2_possible_back_mons: Vec::new(),
            p1_fainted_mons: Vec::new(),
            p2_fainted_mons: Vec::new(),
            p1_unresolved_zoroark_count: 0,
            p2_unresolved_zoroark_count: 0,
            p1_roster_templates: Vec::new(),
            p2_roster_templates: Vec::new(),
            turn_number: 1,
            turn_started: false,
            turn_ended: false,
            p1_has_tera: true,
            p2_has_tera: true,
            p1_has_mega: true,
            p2_has_mega: true,
            weather: None,
            weather_turns: None,
            weather_setter_mon_idx: None,
            pseudo_weathers: Vec::new(),
            pseudo_weather_turns: Vec::new(),
            terrain: None,
            terrain_turns: None,
            terrain_setter_mon_idx: None,
            p1_side_conditions: Vec::new(),
            p1_side_condition_turns: Vec::new(),
            p1_side_condition_setters: Vec::new(),
            p2_side_conditions: Vec::new(),
            p2_side_condition_turns: Vec::new(),
            p2_side_condition_setters: Vec::new(),
            p1_slot_conditions: vec![Vec::new()],
            p2_slot_conditions: vec![Vec::new()],
            self_switch_pending: None,
            items_consumed_this_turn: Vec::new(),
            last_move_on_field: None,
            sub_damage_dealt: 0,
            round_used_this_turn: false,
            predicates: Vec::new(),
        }
    }

    fn p1() -> FieldSlot {
        FieldSlot {
            player: Player::P1,
            slot_index: 0,
        }
    }
    fn o1() -> FieldSlot {
        FieldSlot {
            player: Player::P2,
            slot_index: 0,
        }
    }

    // ── tokenizer / slot / hp / boost primitives ────────────────────────────

    #[test]
    fn slot_parsing_singles_and_indexed() {
        assert_eq!(parse_slot("p1"), Some(p1()));
        assert_eq!(parse_slot("P1"), Some(p1()));
        assert_eq!(parse_slot("o1"), Some(o1()));
        assert_eq!(
            parse_slot("o2"),
            Some(FieldSlot {
                player: Player::P2,
                slot_index: 1
            })
        );
        assert_eq!(parse_slot("x1"), None);
    }

    #[test]
    fn hp_token_parsing() {
        assert!(matches!(parse_hp_token("45%"), Some(HpToken::Percent(45))));
        assert!(matches!(parse_hp_token("97hp"), Some(HpToken::Number(97))));
        assert!(matches!(parse_hp_token("97HP"), Some(HpToken::Number(97))));
        assert!(parse_hp_token("nope").is_none());
    }

    #[test]
    fn boost_token_parsing_both_orders() {
        assert_eq!(parse_boost_token("atk+1"), Some((0, 1)));
        assert_eq!(parse_boost_token("+1atk"), Some((0, 1)));
        assert_eq!(parse_boost_token("spe-2"), Some((4, -2)));
        assert_eq!(parse_boost_token("-2spe"), Some((4, -2)));
        assert_eq!(parse_boost_token("attack+1"), Some((0, 1)));
        assert!(parse_boost_token("garbage").is_none());
    }

    #[test]
    fn hp_event_classifies_direction_against_belief() {
        let belief = test_belief();
        let mut hp_readings = hp_readings_from_belief(&belief);
        // o1 (opponent) starts at 100% — a lower reading is damage.
        let ev = hp_event(&belief, &mut hp_readings, o1(), HpToken::Percent(45));
        assert!(matches!(
            ev.kind,
            EventKind::DamageDealt {
                new_hp: PokemonHP::Percent(45),
                ..
            }
        ));
        // p1 (own) starts at 100 exact — a higher reading is healing.
        let ev = hp_event(&belief, &mut hp_readings, p1(), HpToken::Number(100));
        assert!(matches!(ev.kind, EventKind::SetHp { .. }));
        let ev = hp_event(&belief, &mut hp_readings, p1(), HpToken::Number(120));
        assert!(matches!(
            ev.kind,
            EventKind::Healed {
                new_hp: PokemonHP::Number(120),
                ..
            }
        ));
    }

    // ── casing / abbreviation flexibility ───────────────────────────────────

    #[test]
    fn move_line_is_case_and_punctuation_insensitive() {
        let belief = test_belief();
        for text in [
            "p1 thunderbolt o1 45%",
            "P1 Thunderbolt O1 45%",
            "p1 thunder_bolt o1 45%",
            "p1 Thunder-Bolt o1 45%",
        ] {
            let lines = parse_tracker_text(text, &belief, move_dex(), pokemon_dex())
                .unwrap_or_else(|e| panic!("{text:?} failed to parse: {}", e.message));
            assert_eq!(lines.len(), 1);
            let TrackerLine::Event(ev) = &lines[0] else {
                panic!("expected an event line")
            };
            match &ev.kind {
                EventKind::MoveUsed {
                    move_used, targets, ..
                } => {
                    assert_eq!(*move_used, PokemonMove::Thunderbolt);
                    assert_eq!(targets, &vec![o1()]);
                }
                other => panic!("expected MoveUsed, got {other:?}"),
            }
            assert_eq!(ev.reactions.len(), 1);
            assert!(matches!(
                ev.reactions[0].kind,
                EventKind::DamageDealt { .. }
            ));
        }
    }

    #[test]
    fn endofturn_recognized_case_insensitively() {
        let belief = test_belief();
        for text in ["endofturn", "EndOfTurn", "EOT", "eot"] {
            let lines = parse_tracker_text(text, &belief, move_dex(), pokemon_dex()).unwrap();
            assert_eq!(lines.len(), 1);
            assert!(matches!(lines[0], TrackerLine::EndOfTurn));
        }
    }

    #[test]
    fn eot_prefixed_event_retains_end_of_turn_parentage() {
        let belief = test_belief();
        let lines =
            parse_tracker_text("eot p1 damage 50hp\nendofturn", &belief, move_dex(), pokemon_dex())
                .unwrap();
        assert!(matches!(
            &lines[0],
            TrackerLine::EndOfTurnReaction(InformationEvent {
                kind: EventKind::DamageDealt {
                    target,
                    new_hp: PokemonHP::Number(50),
                    ..
                },
                ..
            }) if *target == p1()
        ));
        assert!(matches!(lines[1], TrackerLine::EndOfTurn));
    }

    /// Regression: several `cant_reason_from_word` words (Taunt, Disable,
    /// Encore, Confusion, Attract, Throat Chop, Torment, Focus Punch,
    /// Gravity, Heal Block) are ALSO real move names. A bare two-token line
    /// (`p1 taunt`) must still parse as the cant-reason (unchanged); a longer
    /// line with a target token (`p1 taunt o1`) must parse as the MOVE being
    /// used, not silently drop the target and misreport a "couldn't act".
    #[test]
    fn move_name_cant_reason_collision_disambiguated_by_line_length() {
        let belief = test_belief();

        let bare = parse_tracker_text("p1 taunt", &belief, move_dex(), pokemon_dex()).unwrap();
        assert!(matches!(
            &bare[0],
            TrackerLine::Event(InformationEvent {
                kind: EventKind::Cant {
                    reason: CantReason::Taunt,
                    ..
                },
                ..
            })
        ));

        let with_target =
            parse_tracker_text("p1 taunt o1", &belief, move_dex(), pokemon_dex()).unwrap();
        let TrackerLine::Event(ev) = &with_target[0] else {
            panic!("expected an event line")
        };
        let EventKind::MoveUsed {
            move_used, targets, ..
        } = &ev.kind
        else {
            panic!("expected MoveUsed, got {:?}", ev.kind)
        };
        assert_eq!(*move_used, PokemonMove::Taunt);
        assert_eq!(targets, &[o1()]);
    }

    /// Every spelling of a charge turn must land on ONE canonical tree: a
    /// `MoveUsed` wrapping a `ChargingMove`, with NO targets.
    ///
    /// The shape matters because it's what the engine itself emits —
    /// `execute_action` folds everything a move emitted into `MoveUsed.reactions`,
    /// so a top-level bare `ChargingMove` never occurs in a battle log and has no
    /// renderer. The empty target list is the user-facing point: on the charge turn
    /// you often can't tell what they aimed at ("Charizard flew up high!" names no
    /// target), so the grammar must not demand one.
    #[test]
    fn charging_spellings_all_produce_one_canonical_tree() {
        let belief = test_belief();

        for text in [
            "o1 solarbeam charging",          // canonical: bare qualifier, no target
            "o1 solarbeam charging solarbeam", // redundant but accepted
            "o1 charging solarbeam",          // standalone alias, desugared
        ] {
            let lines = parse_tracker_text(text, &belief, move_dex(), pokemon_dex())
                .unwrap_or_else(|e| panic!("{text:?} should parse: {e:?}"));
            assert_eq!(lines.len(), 1, "{text:?}");
            let TrackerLine::Event(ev) = &lines[0] else {
                panic!("{text:?}: expected an event line")
            };
            let EventKind::MoveUsed {
                user,
                move_used,
                targets,
            } = &ev.kind
            else {
                panic!("{text:?}: expected MoveUsed, got {:?}", ev.kind)
            };
            assert_eq!(*user, o1(), "{text:?}");
            assert_eq!(*move_used, PokemonMove::SolarBeam, "{text:?}");
            assert!(
                targets.is_empty(),
                "{text:?}: a charge turn must not claim a target, got {targets:?}"
            );
            assert!(
                matches!(
                    ev.reactions.as_slice(),
                    [
                        InformationEvent {
                            kind: EventKind::ChargingMove {
                                move_used: PokemonMove::SolarBeam,
                                ..
                            },
                            ..
                        }
                    ]
                ),
                "{text:?}: expected exactly one ChargingMove reaction, got {:?}",
                ev.reactions
            );
        }
    }

    /// A charge turn must survive parse -> render -> parse unchanged.
    ///
    /// This is what `PUT /api/tracker/{id}/history` does on every edit: it re-renders
    /// the committed events back to text and replays them. Before the canonical
    /// `MoveUsed`-wrapping-`ChargingMove` shape existed, `o1 charging solarbeam`
    /// parsed to a bare top-level `ChargingMove`, which has no top-level renderer —
    /// so authoring a charge turn and then editing any earlier turn hard-failed the
    /// rebuild.
    #[test]
    fn charge_turn_round_trips_through_the_renderer() {
        let belief = test_belief();
        let lines = parse_tracker_text("o1 solarbeam charging", &belief, move_dex(), pokemon_dex())
            .unwrap();
        let TrackerLine::Event(original) = lines.into_iter().next().unwrap() else {
            panic!()
        };

        let text = crate::tracker_render::render_turn(
            std::slice::from_ref(&original),
            &belief,
            move_dex(),
            pokemon_dex(),
        )
        .expect("a charge turn must be renderable");

        let reparsed = parse_tracker_text(&text, &belief, move_dex(), pokemon_dex())
            .unwrap_or_else(|e| panic!("re-parsing {text:?} failed: {e:?}"));
        let TrackerLine::Event(decoded) = &reparsed[0] else {
            panic!("expected an event line from {text:?}")
        };
        assert_eq!(
            format!("{:?}", decoded.kind),
            format!("{:?}", original.kind),
            "rendered as {text:?}"
        );
        assert!(
            matches!(
                decoded.reactions.as_slice(),
                [
                    InformationEvent {
                        kind: EventKind::ChargingMove {
                            move_used: PokemonMove::SolarBeam,
                            ..
                        },
                        ..
                    }
                ]
            ),
            "rendered as {text:?}, decoded reactions = {:?}",
            decoded.reactions
        );
    }

    /// A charge turn where the target IS known (singles, or the release was
    /// obvious) still records it — `charging` is a qualifier, not a target ban.
    #[test]
    fn charging_still_accepts_an_explicit_target() {
        let belief = test_belief();
        let lines =
            parse_tracker_text("p1 fly o1 charging", &belief, move_dex(), pokemon_dex()).unwrap();
        let TrackerLine::Event(ev) = &lines[0] else {
            panic!("expected an event line")
        };
        let EventKind::MoveUsed { targets, .. } = &ev.kind else {
            panic!("expected MoveUsed, got {:?}", ev.kind)
        };
        assert_eq!(targets, &[o1()]);
    }

    /// `charging` naming a DIFFERENT real move is a typo, not a second charge —
    /// a charge turn always charges the move being used, so this is rejected
    /// rather than silently recorded as charging something else.
    #[test]
    fn charging_rejects_a_move_that_is_not_this_lines_move() {
        let belief = test_belief();
        let error = parse_tracker_text(
            "o1 solarbeam charging razorwind",
            &belief,
            move_dex(),
            pokemon_dex(),
        )
        .unwrap_err();
        assert!(
            format!("{error:?}").contains("does not match this line's move"),
            "expected a mismatch error, got {error:?}"
        );
    }

    #[test]
    fn switch_and_ability_and_item_lines() {
        let belief = test_belief();
        let lines = parse_tracker_text(
            "o1 switch garchomp 100%\no1 intimidate\np1 leftovers",
            &belief,
            move_dex(),
            pokemon_dex(),
        )
        .unwrap();
        assert_eq!(lines.len(), 3);
        assert!(matches!(
            &lines[0],
            TrackerLine::Event(InformationEvent {
                kind: EventKind::Switch(_),
                ..
            })
        ));
        assert!(matches!(
            &lines[1],
            TrackerLine::Event(InformationEvent {
                kind: EventKind::AbilityRevealed {
                    ability: Ability::Intimidate,
                    ..
                },
                ..
            })
        ));
        assert!(matches!(
            &lines[2],
            TrackerLine::Event(InformationEvent {
                kind: EventKind::ItemRevealed { .. },
                ..
            })
        ));
    }

    #[test]
    fn leads_line_sends_out_a_whole_side_together() {
        let belief = test_belief();

        // `leads p pikachu` — the viewer's own side; pulls the real exact HP
        // already recorded in the belief (100, per `test_belief`), not a
        // fresh-100%-percent guess.
        let lines =
            parse_tracker_text("leads p pikachu", &belief, move_dex(), pokemon_dex()).unwrap();
        let TrackerLine::Event(ev) = &lines[0] else {
            panic!()
        };
        let EventKind::SimultaneousSwitch { switches } = &ev.kind else {
            panic!("expected SimultaneousSwitch, got {:?}", ev.kind)
        };
        assert_eq!(switches.len(), 1);
        assert_eq!(switches[0].slot, p1());
        assert_eq!(switches[0].species, Species::Pikachu);
        assert!(matches!(switches[0].hp, PokemonHP::Number(100)));

        // `leads o ...` — two species assigned left-to-right to slots 0/1 on
        // the opponent's side, each a fresh 100% send-out.
        let lines = parse_tracker_text(
            "leads o garchomp dragapult",
            &belief,
            move_dex(),
            pokemon_dex(),
        )
        .unwrap();
        let TrackerLine::Event(ev) = &lines[0] else {
            panic!()
        };
        let EventKind::SimultaneousSwitch { switches } = &ev.kind else {
            panic!("expected SimultaneousSwitch, got {:?}", ev.kind)
        };
        assert_eq!(switches.len(), 2);
        assert_eq!(switches[0].species, Species::Garchomp);
        assert_eq!(
            switches[0].slot,
            FieldSlot {
                player: Player::P2,
                slot_index: 0
            }
        );
        assert_eq!(switches[1].species, Species::Dragapult);
        assert_eq!(
            switches[1].slot,
            FieldSlot {
                player: Player::P2,
                slot_index: 1
            }
        );
        assert!(matches!(switches[1].hp, PokemonHP::Percent(100)));
    }

    #[test]
    fn leads_line_combines_both_sides_in_one_line() {
        // The primary new-grammar shape: `leads p <sp>... o <sp>...` on a
        // single line, both sides interleaved via side markers, producing
        // ONE SimultaneousSwitch (no fold needed — the dispatch itself
        // combines both sides).
        let belief = test_belief();
        let lines = parse_tracker_text(
            "leads p tyranitar raichu o charizard aerodactyl",
            &belief,
            move_dex(),
            pokemon_dex(),
        )
        .unwrap();
        assert_eq!(lines.len(), 1);
        let TrackerLine::Event(ev) = &lines[0] else {
            panic!()
        };
        let EventKind::SimultaneousSwitch { switches } = &ev.kind else {
            panic!("expected SimultaneousSwitch, got {:?}", ev.kind)
        };
        assert_eq!(switches.len(), 4);
        assert_eq!(switches[0].species, Species::Tyranitar);
        assert_eq!(switches[0].slot, p1());
        assert_eq!(switches[1].species, Species::Raichu);
        assert_eq!(
            switches[1].slot,
            FieldSlot {
                player: Player::P1,
                slot_index: 1
            }
        );
        assert_eq!(switches[2].species, Species::Charizard);
        assert_eq!(switches[2].slot, o1());
        assert_eq!(switches[3].species, Species::Aerodactyl);
        assert_eq!(
            switches[3].slot,
            FieldSlot {
                player: Player::P2,
                slot_index: 1
            }
        );
    }

    #[test]
    fn fold_leads_and_entry_abilities_merges_both_sides_and_nests_entry_abilities() {
        // Two SEPARATE `leads` lines (one per side) still fold into one
        // combined event, same as a single combined `leads p ... o ...`
        // line would — plus their entry abilities, four consecutive lines
        // with no other event in between.
        let belief = test_belief();
        let lines = parse_tracker_text(
            "leads p tyranitar lucario\nleads o aerodactyl charizard\np1 sandstream\no1 unnerve",
            &belief,
            move_dex(),
            pokemon_dex(),
        )
        .unwrap();
        let events: Vec<InformationEvent> = lines
            .into_iter()
            .map(|l| match l {
                TrackerLine::Event(ev) | TrackerLine::EndOfTurnReaction(ev) => ev,
                TrackerLine::EndOfTurn => panic!("no endofturn in this test"),
            })
            .collect();

        let folded = fold_leads_and_entry_abilities(events);
        assert_eq!(
            folded.len(),
            1,
            "leads (both sides) + their entry abilities should collapse into one event"
        );
        let EventKind::SimultaneousSwitch { switches } = &folded[0].kind else {
            panic!("expected SimultaneousSwitch, got {:?}", folded[0].kind)
        };
        assert_eq!(switches.len(), 4);
        assert_eq!(switches[0].species, Species::Tyranitar);
        assert_eq!(switches[0].slot, p1());
        assert_eq!(switches[1].species, Species::Lucario);
        assert_eq!(
            switches[1].slot,
            FieldSlot {
                player: Player::P1,
                slot_index: 1
            }
        );
        assert_eq!(switches[2].species, Species::Aerodactyl);
        assert_eq!(switches[2].slot, o1());
        assert_eq!(switches[3].species, Species::Charizard);

        let reactions = &folded[0].reactions;
        assert_eq!(
            reactions.len(),
            2,
            "both entry-ability lines should nest as reactions"
        );
        assert!(matches!(
            &reactions[0].kind,
            EventKind::AbilityRevealed { slot, ability: Ability::SandStream } if *slot == p1()
        ));
        assert!(matches!(
            &reactions[1].kind,
            EventKind::AbilityRevealed { slot, ability: Ability::Unnerve } if *slot == o1()
        ));
    }

    #[test]
    fn fold_leaves_unrelated_later_ability_reveal_untouched() {
        // `o1` DID enter via `leads o garchomp` (so it's a `still_entered`
        // slot), but its ability reveal appears AFTER a move has already
        // happened this same turn — that breaks contiguity with the leads
        // block, so it's unrelated to the entry effect and must stay its own
        // top-level event, not get swept into the leads event's reactions.
        let belief = test_belief();
        let lines = parse_tracker_text(
            "leads p pikachu\nleads o garchomp\np1 thunderbolt o1\no1 flashfire",
            &belief,
            move_dex(),
            pokemon_dex(),
        )
        .unwrap();
        let events: Vec<InformationEvent> = lines
            .into_iter()
            .map(|l| match l {
                TrackerLine::Event(ev) | TrackerLine::EndOfTurnReaction(ev) => ev,
                TrackerLine::EndOfTurn => panic!("no endofturn in this test"),
            })
            .collect();

        let folded = fold_leads_and_entry_abilities(events);
        assert_eq!(
            folded.len(),
            3,
            "the move and the later ability reveal stay top-level"
        );
        assert!(matches!(
            folded[0].kind,
            EventKind::SimultaneousSwitch { .. }
        ));
        assert!(matches!(folded[1].kind, EventKind::MoveUsed { .. }));
        assert!(matches!(
            folded[2].kind,
            EventKind::AbilityRevealed {
                ability: Ability::FlashFire,
                ..
            }
        ));
        assert!(
            folded[0].reactions.is_empty(),
            "leads merged from both sides, but no ability lines to fold"
        );
    }

    #[test]
    fn folded_leads_with_entry_ability_apply_to_inference_without_contradiction() {
        // A completely fresh, fully-benched belief — mirrors how
        // `create_tracker` actually seeds a real session (see `tracker.rs`'s
        // module doc: "a session begins fully benched on both sides"),
        // exercising the fold's output through the full
        // fold -> augment -> apply_information pipeline end to end.
        let mut belief = test_belief();
        belief.p1_active_mons = Vec::new();
        belief.p2_active_mons = Vec::new();

        let lines = parse_tracker_text(
            "leads p tyranitar\nleads o gyarados\np1 sandstream\nendofturn",
            &belief,
            move_dex(),
            pokemon_dex(),
        )
        .unwrap();

        let mut turn_events = Vec::new();
        for line in lines {
            match line {
                TrackerLine::Event(ev) | TrackerLine::EndOfTurnReaction(ev) => turn_events.push(ev),
                TrackerLine::EndOfTurn => turn_events.push(leaf(EventKind::EndOfTurn)),
            }
        }

        let folded = fold_leads_and_entry_abilities(turn_events);
        // The combined leads event, plus the trailing EndOfTurn marker.
        assert_eq!(folded.len(), 2);

        let augmented: Vec<InformationEvent> = folded
            .into_iter()
            .map(|e| augment_with_guaranteed_effects(e, &belief, move_dex(), pokemon_dex()))
            .collect();

        let config = InferenceConfig::default();
        let result = apply_information(
            UnknownMatchState::Battle(belief),
            &augmented,
            false,
            pokemon_dex(),
            move_dex(),
            ability_dex(),
            &config,
        );
        let UnknownMatchState::Battle(next) = result else {
            panic!("expected Battle variant")
        };
        assert_eq!(next.weather, Some(Weather::Sandstorm));
        assert!(matches!(
            &next.p1_active_mons[0].possible_abilities,
            poke_rust::information::unknowns::Unknown::Known(a) if *a == Ability::SandStream
        ));
    }

    // ── mega: optional species / suffix shorthand ────────────────────────────

    #[test]
    fn mega_line_auto_fills_single_mega_species() {
        let belief = test_belief_with(Species::Tyranitar, Species::Garchomp);
        let lines = parse_tracker_text("p1 mega", &belief, move_dex(), pokemon_dex()).unwrap();
        let TrackerLine::Event(ev) = &lines[0] else {
            panic!()
        };
        assert!(matches!(
            &ev.kind,
            EventKind::MegaEvolution { into, .. } if *into == Species::TyranitarMega
        ));
    }

    #[test]
    fn mega_line_requires_species_when_ambiguous() {
        let belief = test_belief_with(Species::Charizard, Species::Garchomp);
        let error = parse_tracker_text("p1 mega", &belief, move_dex(), pokemon_dex()).unwrap_err();
        assert!(
            error.message.contains("multiple mega forms"),
            "{}",
            error.message
        );
    }

    #[test]
    fn mega_line_suffix_shorthand_resolves_charizard_y() {
        let belief = test_belief_with(Species::Charizard, Species::Garchomp);
        let lines = parse_tracker_text("p1 mega y", &belief, move_dex(), pokemon_dex()).unwrap();
        let TrackerLine::Event(ev) = &lines[0] else {
            panic!()
        };
        assert!(matches!(
            &ev.kind,
            EventKind::MegaEvolution { into, .. } if *into == Species::CharizardMegaY
        ));
    }

    #[test]
    fn mega_line_full_species_token_still_works() {
        let belief = test_belief_with(Species::Charizard, Species::Garchomp);
        let lines =
            parse_tracker_text("p1 mega charizardmegax", &belief, move_dex(), pokemon_dex())
                .unwrap();
        let TrackerLine::Event(ev) = &lines[0] else {
            panic!()
        };
        assert!(matches!(
            &ev.kind,
            EventKind::MegaEvolution { into, .. } if *into == Species::CharizardMegaX
        ));
    }

    #[test]
    fn mega_resolves_after_same_turn_leads() {
        // Regression: before `slot_species` threading, `mega` read only the
        // frozen pre-submission `belief` — empty-benched for a session that
        // hasn't processed a turn yet (see `tracker.rs`'s module doc) — so a
        // same-turn `leads` line earlier in the SAME submission (which does
        // make the species known) was invisible to a later `mega` line,
        // and `active_species_at` returned `None` for both the auto-fill
        // form (`p1 mega`) and the suffix-shorthand form (`o1 mega y`),
        // exactly the bug report's repro: Tyranitar/Raichu vs.
        // Charizard/Aerodactyl, no Zoroark involved.
        let mut belief = test_belief_with(Species::Tyranitar, Species::Garchomp);
        belief.p1_active_mons = Vec::new();
        belief.p2_active_mons = Vec::new();

        let lines = parse_tracker_text(
            "leads p tyranitar raichu o charizard aerodactyl\np1 mega\no1 mega y",
            &belief,
            move_dex(),
            pokemon_dex(),
        )
        .unwrap();
        assert_eq!(lines.len(), 3);
        let TrackerLine::Event(p1_mega) = &lines[1] else {
            panic!()
        };
        assert!(matches!(
            &p1_mega.kind,
            EventKind::MegaEvolution { into, .. } if *into == Species::TyranitarMega
        ));
        let TrackerLine::Event(o1_mega) = &lines[2] else {
            panic!()
        };
        assert!(matches!(
            &o1_mega.kind,
            EventKind::MegaEvolution { into, .. } if *into == Species::CharizardMegaY
        ));
    }

    // ── guaranteed-effect synthesis ──────────────────────────────────────────

    #[test]
    fn intimidate_reveal_synthesizes_opposing_atk_drop() {
        let belief = test_belief();
        let lines =
            parse_tracker_text("o1 intimidate", &belief, move_dex(), pokemon_dex()).unwrap();
        let TrackerLine::Event(ev) = lines.into_iter().next().unwrap() else {
            panic!()
        };
        let augmented = augment_with_guaranteed_effects(ev, &belief, move_dex(), pokemon_dex());
        assert_eq!(augmented.reactions.len(), 1);
        assert!(matches!(
            augmented.reactions[0].kind,
            EventKind::BoostChanged {
                target,
                boost_idx: 0,
                stages: -1,
            } if target == p1()
        ));
    }

    #[test]
    fn swords_dance_synthesizes_self_boost() {
        let belief = test_belief();
        let lines =
            parse_tracker_text("p1 swordsdance", &belief, move_dex(), pokemon_dex()).unwrap();
        let TrackerLine::Event(ev) = lines.into_iter().next().unwrap() else {
            panic!()
        };
        let augmented = augment_with_guaranteed_effects(ev, &belief, move_dex(), pokemon_dex());
        assert!(augmented.reactions.iter().any(|r| matches!(
            r.kind,
            EventKind::BoostChanged {
                target,
                boost_idx: 0,
                stages,
            } if target == p1() && stages > 0
        )));
    }

    /// A charge turn must not fabricate the RELEASE turn's effects.
    ///
    /// Geomancy is the sharpest case: its +2 SpA/SpD/Spe is the move's ordinary
    /// `self_boost`, which lands when it fires on turn two. Synthesis used to emit it
    /// on the charge turn as well, so a tracked Geomancy user ended up +4/+4/+4.
    #[test]
    fn charge_turn_does_not_synthesize_the_release_turns_self_boost() {
        let belief = test_belief();
        let lines =
            parse_tracker_text("p1 geomancy charging", &belief, move_dex(), pokemon_dex()).unwrap();
        let TrackerLine::Event(ev) = lines.into_iter().next().unwrap() else {
            panic!()
        };
        let augmented = augment_with_guaranteed_effects(ev, &belief, move_dex(), pokemon_dex());
        assert!(
            !augmented
                .reactions
                .iter()
                .any(|r| matches!(r.kind, EventKind::BoostChanged { .. })),
            "Geomancy charges silently — its boosts belong to the release turn; got {:?}",
            augmented.reactions
        );

        // ...and the release turn (no `charging` marker) still gets them.
        let lines = parse_tracker_text("p1 geomancy", &belief, move_dex(), pokemon_dex()).unwrap();
        let TrackerLine::Event(ev) = lines.into_iter().next().unwrap() else {
            panic!()
        };
        let released = augment_with_guaranteed_effects(ev, &belief, move_dex(), pokemon_dex());
        assert!(
            released
                .reactions
                .iter()
                .filter(|r| matches!(r.kind, EventKind::BoostChanged { stages, .. } if stages > 0))
                .count()
                >= 3,
            "Geomancy's release turn must still synthesize +SpA/+SpD/+Spe; got {:?}",
            released.reactions
        );
    }

    /// The moves that DO boost while winding up must have exactly that boost
    /// synthesized — Meteor Beam / Electro Shot +1 SpA, Skull Bash +1 Def — mirroring
    /// `simulator/mod.rs::handle_charging_and_semi_invulnerability`.
    #[test]
    fn charge_turn_synthesizes_the_charge_turn_boost() {
        let belief = test_belief();
        for (text, boost_idx) in [
            ("p1 meteorbeam charging", 2usize),
            ("p1 electroshot charging", 2),
            ("p1 skullbash charging", 1),
        ] {
            let lines = parse_tracker_text(text, &belief, move_dex(), pokemon_dex())
                .unwrap_or_else(|e| panic!("{text:?}: {e:?}"));
            let TrackerLine::Event(ev) = lines.into_iter().next().unwrap() else {
                panic!()
            };
            let augmented = augment_with_guaranteed_effects(ev, &belief, move_dex(), pokemon_dex());
            let boosts: Vec<_> = augmented
                .reactions
                .iter()
                .filter_map(|r| match r.kind {
                    EventKind::BoostChanged {
                        target,
                        boost_idx: i,
                        stages,
                    } => Some((target, i, stages)),
                    _ => None,
                })
                .collect();
            assert_eq!(
                boosts,
                vec![(p1(), boost_idx, 1)],
                "{text:?}: expected exactly one +1 charge-turn boost"
            );
        }
    }

    /// The suppression keys on the typed `charging` marker, NOT on the move carrying a
    /// `charge` flag — which is what makes every skip-the-charge case fall out for
    /// free, with no weather or item modelling in the tracker at all. Power Herb skips
    /// the charge for every two-turn move but Sky Drop, harsh sun skips Solar
    /// Beam's/Solar Blade's, rain skips Electro Shot's; all of them are typed as
    /// ordinary one-turn move lines, so they must still synthesize normally.
    ///
    /// Power Herb Geomancy is the canonical case, and the one where getting this wrong
    /// would hurt most: a suppressed +2/+2/+2 leaves the belief four stages of Speed
    /// behind what's actually on the field.
    #[test]
    fn power_herb_one_turn_use_without_the_marker_still_synthesizes_normally() {
        let belief = test_belief();
        let lines = parse_tracker_text("p1 geomancy", &belief, move_dex(), pokemon_dex()).unwrap();
        let TrackerLine::Event(ev) = lines.into_iter().next().unwrap() else {
            panic!()
        };
        let augmented = augment_with_guaranteed_effects(ev, &belief, move_dex(), pokemon_dex());
        let raised: Vec<_> = augmented
            .reactions
            .iter()
            .filter_map(|r| match r.kind {
                EventKind::BoostChanged {
                    boost_idx, stages, ..
                } if stages > 0 => Some(boost_idx),
                _ => None,
            })
            .collect();
        assert_eq!(
            raised.len(),
            3,
            "Power Herb Geomancy resolves in one turn and must still get its \
             +SpA/+SpD/+Spe; got {:?}",
            augmented.reactions
        );
    }

    /// Meteor Beam's hardcoded charge boost still occurs when Power Herb makes
    /// it a one-turn move. Pure charge turns are now recorded on the belief, so
    /// a later release can be distinguished and will not synthesize it twice.
    #[test]
    fn hardcoded_charge_boost_is_synthesized_for_a_one_turn_use() {
        let belief = test_belief();
        let lines =
            parse_tracker_text("p1 meteorbeam o1", &belief, move_dex(), pokemon_dex()).unwrap();
        let TrackerLine::Event(ev) = lines.into_iter().next().unwrap() else {
            panic!()
        };
        let augmented = augment_with_guaranteed_effects(ev, &belief, move_dex(), pokemon_dex());
        assert!(
            augmented.reactions.iter().any(|r| matches!(
                r.kind,
                EventKind::BoostChanged {
                    target,
                    boost_idx: 2,
                    stages: 1
                } if target == p1()
            )),
            "one-turn Meteor Beam should retain its charge boost; got {:?}",
            augmented.reactions
        );
    }

    /// Regression for the "volatiles should use default time amounts, AND be
    /// decremented on endofturn" gap: Syrup Bomb's `syrupbomb` volatile (a
    /// real 3-turn volatile with no item-based extension, per `volatile_timer`
    /// in `inference.rs`) must seed at exactly 3 turns when applied and count
    /// down 3 -> 2 -> 1 -> removed across three EndOfTurns, matching the real
    /// duration exactly (a real observer can count this precisely — no
    /// ambiguity to range over, unlike weather). Deliberately NOT Taunt/
    /// Disable/Encore/ThroatChop — those words collide with
    /// `cant_reason_from_word` (checked before move dispatch in `parse_line`),
    /// a separate, pre-existing grammar ambiguity outside this fix's scope.
    #[test]
    fn syrup_bomb_volatile_seeds_and_decrements_across_end_of_turns() {
        let belief = test_belief();
        let lines = parse_tracker_text(
            "p1 syrupbomb o1\nendofturn",
            &belief,
            move_dex(),
            pokemon_dex(),
        )
        .unwrap();
        let mut events = Vec::new();
        for line in lines {
            match line {
                TrackerLine::Event(ev) | TrackerLine::EndOfTurnReaction(ev) => events.push(augment_with_guaranteed_effects(
                    ev,
                    &belief,
                    move_dex(),
                    pokemon_dex(),
                )),
                TrackerLine::EndOfTurn => events.push(leaf(EventKind::EndOfTurn)),
            }
        }
        let config = InferenceConfig::default();
        let mut state = apply_information(
            UnknownMatchState::Battle(belief),
            &events,
            false,
            pokemon_dex(),
            move_dex(),
            ability_dex(),
            &config,
        );

        let syrup_bomb_turns = |state: &UnknownMatchState| -> Option<u16> {
            let UnknownMatchState::Battle(b) = state else {
                panic!("expected Battle variant")
            };
            b.p2_active_mons[0].volatiles.iter().find_map(|v| match v {
                poke_rust::state::pokemon::VolatileStatusState::TurnStatus(
                    poke_rust::state::dex_data::VolatileStatus::SyrupBomb,
                    turns,
                ) => Some(*turns),
                _ => None,
            })
        };

        // Syrup Bomb seeds at 3, but the SAME turn's own `endofturn` (already
        // included above, matching how a real turn resolves) immediately
        // decrements it once — 3 -> 2 — exactly mirroring the concrete
        // simulator's own `syrup_bomb_drops_speed_each_turn_for_3_turns` test
        // (seed 3, three total EOT decrements to reach removal).
        assert_eq!(
            syrup_bomb_turns(&state),
            Some(2),
            "Syrup Bomb's volatile should have decremented once by its own use-turn's endofturn"
        );

        // Two more bare EndOfTurns: 2 -> 1 -> removed (three EOTs total since seeding).
        for expected in [Some(1), None] {
            state = apply_information(
                state,
                &[leaf(EventKind::EndOfTurn)],
                false,
                pokemon_dex(),
                move_dex(),
                ability_dex(),
                &config,
            );
            assert_eq!(syrup_bomb_turns(&state), expected);
        }
    }

    // ── full pipeline: parse -> augment -> apply_information, no panic ──────

    #[test]
    fn full_turn_applies_to_inference_without_contradiction() {
        let belief = test_belief();
        let lines = parse_tracker_text(
            "p1 thunderbolt o1 45%\nendofturn",
            &belief,
            move_dex(),
            pokemon_dex(),
        )
        .unwrap();

        let mut events = Vec::new();
        for line in lines {
            match line {
                TrackerLine::Event(ev) | TrackerLine::EndOfTurnReaction(ev) => events.push(augment_with_guaranteed_effects(
                    ev,
                    &belief,
                    move_dex(),
                    pokemon_dex(),
                )),
                TrackerLine::EndOfTurn => events.push(leaf(EventKind::EndOfTurn)),
            }
        }

        let config = InferenceConfig::default();
        let result = apply_information(
            UnknownMatchState::Battle(belief),
            &events,
            false,
            pokemon_dex(),
            move_dex(),
            ability_dex(),
            &config,
        );
        let UnknownMatchState::Battle(next) = result else {
            panic!("expected Battle variant")
        };
        // The opponent's HP observation should have moved from 100% to 45%.
        assert!(matches!(next.p2_active_mons[0].hp, PokemonHP::Percent(45)));
    }

    /// Parse `text` against `belief`, augment, and run it through
    /// `apply_information`, returning the resulting belief. Shared by the
    /// reactive item/ability tests below.
    fn run_turn(text: &str, belief: UnknownBattleState) -> UnknownBattleState {
        let lines = parse_tracker_text(text, &belief, move_dex(), pokemon_dex()).unwrap();
        let mut events = Vec::new();
        for line in lines {
            match line {
                TrackerLine::Event(ev) | TrackerLine::EndOfTurnReaction(ev) => events.push(augment_with_guaranteed_effects(
                    ev,
                    &belief,
                    move_dex(),
                    pokemon_dex(),
                )),
                TrackerLine::EndOfTurn => events.push(leaf(EventKind::EndOfTurn)),
            }
        }
        let config = InferenceConfig::default();
        let result = apply_information(
            UnknownMatchState::Battle(belief),
            &events,
            false,
            pokemon_dex(),
            move_dex(),
            ability_dex(),
            &config,
        );
        let UnknownMatchState::Battle(next) = result else {
            panic!("expected Battle variant")
        };
        next
    }

    // ── reactive item/ability mentions on a move line ────────────────────────
    // These are typed by the user (not guaranteed-synthesized — see
    // `tracker_effects.rs`'s scope note) but must still parse into the right
    // slot and actually reveal the information in the resulting belief.

    #[test]
    fn rocky_helmet_reveal_and_recoil_chip_are_both_recorded() {
        let belief = test_belief();
        // o1 (defender) revealed at 50% with Rocky Helmet; p1 (attacker) then
        // takes the chip damage down to 97 exact HP.
        let next = run_turn("p1 tackle o1 50% rockyhelmet p1 97hp\nendofturn", belief);
        assert!(matches!(
            &next.p2_active_mons[0].item,
            poke_rust::information::unknowns::Unknown::Known(Item::RockyHelmet)
        ));
        assert!(matches!(next.p1_active_mons[0].hp, PokemonHP::Number(97)));
    }

    #[test]
    fn rough_skin_reveal_and_recoil_chip_are_both_recorded() {
        let belief = test_belief();
        let next = run_turn("p1 tackle o1 50% roughskin p1 97hp\nendofturn", belief);
        assert!(matches!(
            &next.p2_active_mons[0].possible_abilities,
            poke_rust::information::unknowns::Unknown::Known(Ability::RoughSkin)
        ));
        assert!(matches!(next.p1_active_mons[0].hp, PokemonHP::Number(97)));
    }

    #[test]
    fn life_orb_reveal_and_self_recoil_are_both_recorded() {
        let belief = test_belief();
        // p1 (attacker) reveals Life Orb, then takes its own recoil down to 88.
        // "p1" must be re-mentioned before "lifeorb" per the flat-attachment
        // rule (current slot is o1 right after the "45%" token otherwise).
        let next = run_turn("p1 thunderbolt o1 45% p1 lifeorb 88hp\nendofturn", belief);
        assert!(matches!(
            &next.p1_active_mons[0].item,
            poke_rust::information::unknowns::Unknown::Known(Item::LifeOrb)
        ));
        assert!(matches!(next.p1_active_mons[0].hp, PokemonHP::Number(88)));
    }

    #[test]
    fn sitrus_berry_bare_mention_reveals_item_but_does_not_mark_it_consumed() {
        // A bare item mention (no verb) is a passive reveal, not a
        // consumption — this documents that distinction deliberately, not as
        // a gap: use `consumes`/`ate`/etc. (next test) to record it as gone.
        let belief = test_belief();
        let next = run_turn("p1 tackle o1 40% sitrus 65%\nendofturn", belief);
        assert!(matches!(
            &next.p2_active_mons[0].item,
            poke_rust::information::unknowns::Unknown::Known(Item::SitrusBerry)
        ));
        assert!(matches!(next.p2_active_mons[0].hp, PokemonHP::Percent(65)));
        assert!(!next.p2_active_mons[0].item_lost);
        assert!(next.p2_active_mons[0].consumed_item.is_none());
    }

    #[test]
    fn sitrus_berry_consumed_verb_marks_it_gone() {
        let belief = test_belief();
        // o1 drops to 40%, its Sitrus Berry procs (explicitly "ate"), healing
        // back to 65% — the belief should show it consumed, not still held.
        let next = run_turn("p1 tackle o1 40% ate sitrus 65%\nendofturn", belief);
        assert_eq!(
            next.p2_active_mons[0].consumed_item,
            Some(Item::SitrusBerry)
        );
        assert!(matches!(next.p2_active_mons[0].hp, PokemonHP::Percent(65)));
    }

    // ── recoil / drain: not auto-synthesized, but accepted + applied when typed ──
    // (The tracker can't reliably back out an exact recoil/drain fraction from
    // a percent-based foe-damage observation onto the attacker's own HP scale,
    // so — unlike move secondaries — these stay user-typed on both sides.)

    #[test]
    fn recoil_move_self_damage_is_accepted_when_typed() {
        let belief = test_belief();
        let next = run_turn("p1 bravebird o1 40% p1 88hp\nendofturn", belief);
        assert!(matches!(next.p2_active_mons[0].hp, PokemonHP::Percent(40)));
        assert!(matches!(next.p1_active_mons[0].hp, PokemonHP::Number(88)));
    }

    #[test]
    fn drain_move_self_heal_is_accepted_when_typed() {
        let belief = test_belief();
        // p1 starts at exact 100 HP; a value above that unambiguously
        // classifies as Healed rather than SetHp/DamageDealt.
        let next = run_turn("p1 drainingkiss o1 60% p1 120hp\nendofturn", belief);
        assert!(matches!(next.p2_active_mons[0].hp, PokemonHP::Percent(60)));
        assert!(matches!(next.p1_active_mons[0].hp, PokemonHP::Number(120)));
    }

    // ── chance-based (non-guaranteed) secondaries ────────────────────────────

    #[test]
    fn chance_based_secondary_is_not_auto_applied() {
        // Scald's burn is a 30%-chance secondary, not a guaranteed one — the
        // generic secondary synthesis (see tracker_effects.rs) must only fire
        // at chance == 100, unlike Thunder Wave's guaranteed Paralysis.
        let belief = test_belief();
        let lines = parse_tracker_text(
            "p1 scald o1 50%\nendofturn",
            &belief,
            move_dex(),
            pokemon_dex(),
        )
        .unwrap();
        let TrackerLine::Event(ev) = lines.into_iter().next().unwrap() else {
            panic!()
        };
        let augmented = augment_with_guaranteed_effects(ev, &belief, move_dex(), pokemon_dex());
        assert!(
            !augmented
                .reactions
                .iter()
                .any(|r| matches!(&r.kind, EventKind::StatusInflicted { .. }))
        );
    }

    #[test]
    fn chance_based_secondary_is_recorded_and_revealed_when_typed() {
        let belief = test_belief();
        let next = run_turn("p1 scald o1 50% burn\nendofturn", belief);
        assert!(matches!(
            next.p2_active_mons[0].status,
            Some(poke_rust::state::dex_data::Status::Burn)
        ));
    }

    // ── item-interaction moves ────────────────────────────────────────────

    #[test]
    fn knock_off_removes_targets_item() {
        let belief = test_belief();
        let next = run_turn("p1 knockoff o1 45% loses leftovers\nendofturn", belief);
        assert_eq!(next.p2_active_mons[0].removed_item, Some(Item::Leftovers));
    }

    #[test]
    fn bug_bite_eats_targets_berry() {
        let belief = test_belief();
        let next = run_turn("p1 bugbite o1 45% loses sitrus\nendofturn", belief);
        assert_eq!(next.p2_active_mons[0].removed_item, Some(Item::SitrusBerry));
    }

    #[test]
    fn bug_bite_attacker_gaining_the_berrys_effect_is_accepted_when_typed() {
        // The berry's effect landing on the *attacker* (not its original
        // holder) is never synthesized — re-mentioning the attacker after
        // the item-loss token lets the user record it manually, same as any
        // other self-directed HP token.
        let belief = test_belief();
        let next = run_turn("p1 bugbite o1 45% loses sitrus p1 100hp\nendofturn", belief);
        assert_eq!(next.p2_active_mons[0].removed_item, Some(Item::SitrusBerry));
        assert!(matches!(next.p1_active_mons[0].hp, PokemonHP::Number(100)));
    }

    // ── blocked targets don't get guaranteed effects either ──────────────────

    #[test]
    fn blocked_target_does_not_get_guaranteed_status() {
        let belief = test_belief();
        let lines = parse_tracker_text(
            "p1 thunderwave o1 blocked\nendofturn",
            &belief,
            move_dex(),
            pokemon_dex(),
        )
        .unwrap();
        let TrackerLine::Event(ev) = lines.into_iter().next().unwrap() else {
            panic!()
        };
        let augmented = augment_with_guaranteed_effects(ev, &belief, move_dex(), pokemon_dex());
        assert!(
            !augmented
                .reactions
                .iter()
                .any(|r| matches!(&r.kind, EventKind::StatusInflicted { .. }))
        );
    }

    // ── multi-hit moves ──────────────────────────────────────────────────────

    #[test]
    fn multi_hit_move_applies_sequential_damage_to_the_same_target() {
        let belief = test_belief();
        let next = run_turn("p1 iciclespear o1 70% o1 40%\nendofturn", belief);
        assert!(matches!(next.p2_active_mons[0].hp, PokemonHP::Percent(40)));
    }

    fn randomized_case(input: &str, rng: &mut StdRng) -> String {
        input
            .chars()
            .map(|ch| {
                if ch.is_ascii_alphabetic() && rng.gen_bool(0.5) {
                    ch.to_ascii_uppercase()
                } else {
                    ch.to_ascii_lowercase()
                }
            })
            .collect()
    }

    /// Property-style grammar sweep. Each seed cycles through every major
    /// line family while randomizing case, aliases, values, and qualifiers;
    /// failures report the exact seed and generated text for replay.
    #[test]
    fn randomized_tracker_grammar_parses_supported_surface_forms() {
        let iterations = std::env::var("POKERUST_TRACKER_GRAMMAR_FUZZ_ITERS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(512);
        let seed_start = std::env::var("POKERUST_TRACKER_GRAMMAR_FUZZ_SEED_START")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(0);
        let belief = test_belief();

        for seed in seed_start..seed_start.saturating_add(iterations) {
            let mut rng = StdRng::seed_from_u64(seed);
            let family = seed % 10;
            let (text, check): (String, fn(&TrackerLine) -> bool) = match family {
                0 => {
                    let hp = rng.gen_range(1..100);
                    (
                        format!(
                            "p1 Thunder-Bolt o1 {hp}% o1 {}",
                            if rng.gen_bool(0.5) { "def-1" } else { "-1def" }
                        ),
                        |line| {
                            matches!(
                                line,
                                TrackerLine::Event(InformationEvent {
                                    kind: EventKind::MoveUsed {
                                        move_used: PokemonMove::Thunderbolt,
                                        ..
                                    },
                                    ..
                                })
                            )
                        },
                    )
                }
                1 => (
                    format!(
                        "o1 {} garchomp 73%",
                        ["switch", "switchin", "sendout"][rng.gen_range(0..3)]
                    ),
                    |line| {
                        matches!(
                            line,
                            TrackerLine::Event(InformationEvent {
                                kind: EventKind::Switch(_),
                                ..
                            })
                        )
                    },
                ),
                2 => (
                    "leads p pikachu charizard".to_string(),
                    |line| matches!(line, TrackerLine::Event(InformationEvent { kind: EventKind::SimultaneousSwitch { switches }, .. }) if switches.len() == 2),
                ),
                3 => (
                    format!(
                        "weather {}",
                        ["rain", "sun", "sand", "snow", "clear"][rng.gen_range(0..5)]
                    ),
                    |line| {
                        matches!(
                            line,
                            TrackerLine::Event(InformationEvent {
                                kind: EventKind::WeatherChanged { .. },
                                ..
                            })
                        )
                    },
                ),
                4 => (
                    format!(
                        "terrain {}",
                        ["electric", "grassy", "misty", "psychic", "none"][rng.gen_range(0..5)]
                    ),
                    |line| {
                        matches!(
                            line,
                            TrackerLine::Event(InformationEvent {
                                kind: EventKind::TerrainChanged { .. },
                                ..
                            })
                        )
                    },
                ),
                5 => (
                    format!(
                        "p1 {} electric",
                        ["tera", "terastallize", "terastallized"][rng.gen_range(0..3)]
                    ),
                    |line| {
                        matches!(
                            line,
                            TrackerLine::Event(InformationEvent {
                                kind: EventKind::Terastallization { .. },
                                ..
                            })
                        )
                    },
                ),
                6 => (
                    format!(
                        "o1 {} sitrus",
                        ["loses", "consumes", "ate", "gains"][rng.gen_range(0..4)]
                    ),
                    |line| {
                        matches!(
                            line,
                            TrackerLine::Event(InformationEvent {
                                kind: EventKind::ItemLost { .. } | EventKind::ItemGained { .. },
                                ..
                            })
                        )
                    },
                ),
                7 => (format!("p1 hp {}hp", rng.gen_range(0..151)), |line| {
                    matches!(
                        line,
                        TrackerLine::Event(InformationEvent {
                            kind: EventKind::DamageDealt { .. }
                                | EventKind::Healed { .. }
                                | EventKind::SetHp { .. },
                            ..
                        })
                    )
                }),
                8 => (
                    format!(
                        "o1 {}",
                        ["flinch", "fullpara", "sleep", "taunt", "encore", "pass"]
                            [rng.gen_range(0..6)]
                    ),
                    |line| {
                        matches!(
                            line,
                            TrackerLine::Event(InformationEvent {
                                kind: EventKind::Cant { .. },
                                ..
                            })
                        )
                    },
                ),
                _ => (
                    ["endofturn", "eot"][rng.gen_range(0..2)].to_string(),
                    |line| matches!(line, TrackerLine::EndOfTurn),
                ),
            };
            let text = randomized_case(&text, &mut rng);
            let parsed = parse_tracker_text(&text, &belief, move_dex(), pokemon_dex())
                .unwrap_or_else(|error| {
                    panic!(
                        "grammar fuzz seed={seed} family={family} failed at line {}: {}\n{text}",
                        error.line, error.message
                    )
                });
            assert_eq!(parsed.len(), 1, "grammar fuzz seed={seed}: {text}");
            assert!(
                check(&parsed[0]),
                "grammar fuzz seed={seed}: {text}\n{:#?}",
                parsed[0]
            );
        }
    }
}
