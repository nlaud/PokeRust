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
//!   singles auto-target inference.
//! - **Leads are an event, not a pre-game pick.** `[p|o] leads <species>...`
//!   sends out a whole side's opening (or simultaneous post-faint
//!   replacement) leads together — a session starts fully benched on both
//!   sides (see `tracker.rs`'s module doc), symmetric with how every other
//!   mid-battle switch already works. Distinct from `switch`, which replaces
//!   one slot at a time.
//! - **HP direction from the belief.** `[xx]%`/`[xx]hp` tokens don't say
//!   whether they're damage or healing — that's inferred by comparing against
//!   the slot's currently-believed HP. Equal-to-current is emitted as `SetHp`
//!   (no mechanism implied).
//! - Guaranteed-effect synthesis (Intimidate's `-1 atk`, Swords Dance's
//!   `+2 atk`, weather from Drizzle, …) lives in `tracker_effects.rs` and is
//!   applied as a post-processing pass over the events this module builds —
//!   see `crate::tracker_effects::augment_with_guaranteed_effects`.

use std::collections::HashMap;

use poke_rust::data::ability::Ability;
use poke_rust::data::item::Item;
use poke_rust::data::pokemon_move::PokemonMove;
use poke_rust::data::species::Species;
use poke_rust::information::information::{CantReason, EventKind, InformationEvent, SwitchState};
use poke_rust::information::unknowns::{PokemonHP, UnknownBattleState};
use poke_rust::state::battle::{FieldSlot, Player};
use poke_rust::state::dex_data::{
    MoveData, PokemonData, PokemonType, Status, Terrain, VolatileStatus, Weather,
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
    EndOfTurn,
}

/// Parse every line of `text` into `TrackerLine`s. Blank lines and lines
/// starting with `#` are ignored. `belief` is read (never mutated) to resolve
/// HP-direction (damage vs. heal vs. unchanged) for `hpspec` tokens.
pub fn parse_tracker_text(
    text: &str,
    belief: &UnknownBattleState,
    move_dex: &HashMap<PokemonMove, MoveData>,
    pokemon_dex: &HashMap<Species, PokemonData>,
) -> Result<Vec<TrackerLine>, ParseError> {
    let mut out = Vec::new();
    for (idx, raw_line) in text.lines().enumerate() {
        let line_no = idx + 1;
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let tokens: Vec<&str> = line.split_whitespace().collect();
        out.push(parse_line(&tokens, line_no, belief, move_dex, pokemon_dex)?);
    }
    Ok(out)
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

pub(crate) fn opposing_active_slots(belief: &UnknownBattleState, slot: FieldSlot) -> Vec<FieldSlot> {
    let (opp, count) = match slot.player {
        Player::P1 => (Player::P2, belief.p2_active_mons.len()),
        Player::P2 => (Player::P1, belief.p1_active_mons.len()),
    };
    (0..count as u8)
        .map(|i| FieldSlot {
            player: opp,
            slot_index: i,
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

fn current_hp(belief: &UnknownBattleState, slot: FieldSlot) -> Option<PokemonHP> {
    let mons = match slot.player {
        Player::P1 => &belief.p1_active_mons,
        Player::P2 => &belief.p2_active_mons,
    };
    mons.get(slot.slot_index as usize).map(|m| m.hp.clone())
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
fn hp_event(belief: &UnknownBattleState, slot: FieldSlot, token: HpToken) -> InformationEvent {
    let is_own = slot.player == Player::P1;
    let (new_hp, went_down, went_up) = match (current_hp(belief, slot), token) {
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
    let max_hp = if is_own { known_max_hp(belief, slot) } else { 0 };
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
            let digit_end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
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
) -> Result<TrackerLine, ParseError> {
    if norm(tokens[0]) == "endofturn" || norm(tokens[0]) == "eot" {
        return Ok(TrackerLine::EndOfTurn);
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
        _ => {}
    }

    let slot = parse_slot(tokens[0])
        .ok_or_else(|| err(line_no, format!("expected a slot (p1/o1/…), got '{}'", tokens[0])))?;
    let action = tokens
        .get(1)
        .ok_or_else(|| err(line_no, "expected an action after the slot"))?;
    let action_n = norm(action);

    // ── switch ───────────────────────────────────────────────────────────
    // A single mid-battle replacement for one slot. For a whole side's
    // opening (or post-faint replacement) leads sent out together, use
    // `leads` instead — see its handler below.
    if action_n == "switch" || action_n == "switchin" || action_n == "sendout" {
        let species_tok = tokens
            .get(2)
            .ok_or_else(|| err(line_no, "switch requires a species"))?;
        let species = Species::from_str(species_tok);
        if matches!(species, Species::Unknown(_)) {
            return Err(err(line_no, format!("unrecognized species '{species_tok}'")));
        }
        let mut switch = build_switch_state(belief, slot, species);
        // An explicit trailing HP token overrides the default (exact-known-max
        // for the viewer, 100% for the opponent) `build_switch_state` assumes.
        if let Some(tok) = tokens.get(3)
            && let Some(hp_tok) = parse_hp_token(tok)
        {
            switch.hp = match hp_tok {
                HpToken::Percent(p) => PokemonHP::Percent(p),
                HpToken::Number(n) => PokemonHP::Number(n),
            };
        }
        return Ok(TrackerLine::Event(InformationEvent {
            kind: EventKind::Switch(switch),
            reactions: Vec::new(),
        }));
    }

    // ── leads ────────────────────────────────────────────────────────────
    // `[p|o] leads <species>...` — a whole side's battle-start (or
    // simultaneous post-faint replacement) leads, sent out together. The slot
    // digit on the address token (if any) is ignored — `leads` always
    // addresses the WHOLE side, left-to-right (slot 0 = leftmost), not one
    // slot; write `p leads charizard sylveon`, not `p1 leads …`/`p2 leads …`.
    if action_n == "leads" {
        let species_toks = &tokens[2..];
        if species_toks.is_empty() {
            return Err(err(line_no, "leads requires at least one species"));
        }
        let mut switches = Vec::with_capacity(species_toks.len());
        for (i, tok) in species_toks.iter().enumerate() {
            let species = Species::from_str(tok);
            if matches!(species, Species::Unknown(_)) {
                return Err(err(line_no, format!("unrecognized species '{tok}'")));
            }
            let lead_slot = FieldSlot {
                player: slot.player,
                slot_index: i as u8,
            };
            switches.push(build_switch_state(belief, lead_slot, species));
        }
        return Ok(TrackerLine::Event(InformationEvent {
            kind: EventKind::SimultaneousSwitch { switches },
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
                let species = active_species_at(belief, slot).ok_or_else(|| {
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
                    let species = active_species_at(belief, slot).ok_or_else(|| {
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
                                format!("unrecognized species or ambiguous mega suffix '{species_tok}'"),
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
    if let Some(reason) = cant_reason_from_word(&action_n) {
        return Ok(TrackerLine::Event(InformationEvent {
            kind: EventKind::Cant { slot, reason },
            reactions: Vec::new(),
        }));
    }

    // ── move ─────────────────────────────────────────────────────────────
    let candidate_move = PokemonMove::from_str(action);
    if move_dex.contains_key(&candidate_move) {
        return parse_move_line(slot, candidate_move, &tokens[2..], line_no, belief);
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

/// The species currently occupying `slot`, when it's already `Known` in the
/// belief (an opponent's active is only `Known` once revealed by an earlier
/// switch/reveal; the viewer's own side is always `Known`). Used by the
/// `mega` line to look up which mega forms are even possible.
fn active_species_at(belief: &UnknownBattleState, slot: FieldSlot) -> Option<Species> {
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
fn build_switch_state(belief: &UnknownBattleState, slot: FieldSlot, species: Species) -> SwitchState {
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
) -> Result<TrackerLine, ParseError> {
    let mut targets: Vec<FieldSlot> = Vec::new();
    let mut children: Vec<InformationEvent> = Vec::new();
    let mut current = user;

    let mut i = 0;
    while i < rest.len() {
        let tok = rest[i];
        let n = norm(tok);

        if let Some(s) = parse_slot(tok) {
            current = s;
            if !targets.contains(&s) && s != user {
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
        } else if let Some(hp) = parse_hp_token(tok) {
            children.push(hp_event(belief, current, hp));
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

    fn make_active(species: Species, hp: PokemonHP) -> poke_rust::information::unknowns::UnknownPokemonState {
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
        // o1 (opponent) starts at 100% — a lower reading is damage.
        let ev = hp_event(&belief, o1(), HpToken::Percent(45));
        assert!(matches!(
            ev.kind,
            EventKind::DamageDealt {
                new_hp: PokemonHP::Percent(45),
                ..
            }
        ));
        // p1 (own) starts at 100 exact — a higher reading is healing.
        let ev = hp_event(&belief, p1(), HpToken::Number(100));
        assert!(matches!(ev.kind, EventKind::SetHp { .. }));
        let ev = hp_event(&belief, p1(), HpToken::Number(120));
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
            assert!(matches!(ev.reactions[0].kind, EventKind::DamageDealt { .. }));
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

        // `p leads pikachu` — the viewer's own side; pulls the real exact HP
        // already recorded in the belief (100, per `test_belief`), not a
        // fresh-100%-percent guess.
        let lines = parse_tracker_text("p leads pikachu", &belief, move_dex(), pokemon_dex()).unwrap();
        let TrackerLine::Event(ev) = &lines[0] else { panic!() };
        let EventKind::SimultaneousSwitch { switches } = &ev.kind else {
            panic!("expected SimultaneousSwitch, got {:?}", ev.kind)
        };
        assert_eq!(switches.len(), 1);
        assert_eq!(switches[0].slot, p1());
        assert_eq!(switches[0].species, Species::Pikachu);
        assert!(matches!(switches[0].hp, PokemonHP::Number(100)));

        // `o leads ...` — two species assigned left-to-right to slots 0/1 on
        // the opponent's side, each a fresh 100% send-out.
        let lines =
            parse_tracker_text("o leads garchomp dragapult", &belief, move_dex(), pokemon_dex())
                .unwrap();
        let TrackerLine::Event(ev) = &lines[0] else { panic!() };
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
        assert!(error.message.contains("multiple mega forms"), "{}", error.message);
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
            parse_tracker_text("p1 mega charizardmegax", &belief, move_dex(), pokemon_dex()).unwrap();
        let TrackerLine::Event(ev) = &lines[0] else {
            panic!()
        };
        assert!(matches!(
            &ev.kind,
            EventKind::MegaEvolution { into, .. } if *into == Species::CharizardMegaX
        ));
    }

    // ── guaranteed-effect synthesis ──────────────────────────────────────────

    #[test]
    fn intimidate_reveal_synthesizes_opposing_atk_drop() {
        let belief = test_belief();
        let lines = parse_tracker_text("o1 intimidate", &belief, move_dex(), pokemon_dex()).unwrap();
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
        let lines = parse_tracker_text("p1 swordsdance", &belief, move_dex(), pokemon_dex()).unwrap();
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
                TrackerLine::Event(ev) => {
                    events.push(augment_with_guaranteed_effects(
                        ev,
                        &belief,
                        move_dex(),
                        pokemon_dex(),
                    ))
                }
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
                TrackerLine::Event(ev) => events.push(augment_with_guaranteed_effects(
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
        let lines =
            parse_tracker_text("p1 scald o1 50%\nendofturn", &belief, move_dex(), pokemon_dex())
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
}
