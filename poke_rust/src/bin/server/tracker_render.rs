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
//! guessing at unsupported syntax. The fuzz test below SKIPS (doesn't fail)
//! any battle turn containing an unsupported event — this is honest,
//! bounded coverage of the paths that ARE typeable today, not full coverage
//! of the engine's entire event vocabulary. Expanding grammar coverage is a
//! separate, follow-on effort (see the plan file / TODO.md).
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
//! Not yet wired into any HTTP route — today this module exists solely to
//! drive `tracker_round_trip_stays_a_sound_superset_of_the_real_battle`
//! below, hence the blanket `dead_code` allow (a normal, non-test build has
//! no other caller). A natural next step is exposing this as a real
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
/// short human-readable reason so a fuzz-test skip can explain itself.
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
            continue; // added once, at the end, below.
        }
        if let Some(line) = render_top_level_event(event, belief, move_dex, pokemon_dex)? {
            lines.push(line);
        }
    }
    lines.push("endofturn".to_string());
    Ok(lines.join("\n"))
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
                let _ = write!(line, " {}", render_move_qualifier(r)?);
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
                let side: Vec<&SwitchState> =
                    switches.iter().filter(|sw| sw.slot.player == player).collect();
                if side.is_empty() {
                    continue;
                }
                let species_list = side
                    .iter()
                    .map(|sw| species_word(&sw.species))
                    .collect::<Vec<_>>()
                    .join(" ");
                out.push(format!("{} leads {}", side_word(player), species_list));
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
fn render_move_qualifier(event: &InformationEvent) -> Result<String, Unsupported> {
    match &event.kind {
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
        other => Err(unsupported(format!(
            "{other:?} has no inline move-qualifier tracker grammar yet"
        ))),
    }
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
    //! This is deliberately a small, hand-authored set of scenarios rather
    //! than the engine's own large randomized battle generator
    //! (`random_battle_tests.rs`) — that generator's random-command helpers
    //! are private to the library's own test module and not reachable from
    //! this binary crate, and reimplementing an equally thorough random
    //! generator was out of scope for this pass. What IS exercised here is
    //! the full, real pipeline (render_turn's diffing-against-synthesis
    //! logic, `fold_leads_and_entry_abilities`, `augment_turn`, and
    //! `apply_information`) against genuine simulated data, covering: a lead
    //! entry-ability weather trigger (Sand Stream), a guaranteed-secondary
    //! status move (Thunder Wave), and an ordinary damaging move. Expanding
    //! this into a true randomized round-trip sweep is a natural follow-on
    //! once `render_turn`'s grammar coverage grows past this pass's scope
    //! (see this module's doc comment).
    use super::*;
    use poke_rust::information::inference::{InferenceConfig, apply_information};
    use poke_rust::information::information::mask_events_for;
    use poke_rust::information::subset_check::assert_true_state_subset_of_belief;
    use poke_rust::information::unknowns::UnknownMatchState;
    use poke_rust::state::battle::{
        AttackCommand, BattleCommand, MatchState, PlayerCommand, TeamPreviewCommand,
    };
    use poke_rust::state::dex_data::{parse_ability_dex, parse_move_dex, parse_pokemon_dex, AbilityData};
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

    const TEAM_P1: &str = "Pikachu @ Light Ball\nAbility: Static\nLevel: 50\nEVs: 4 HP / 252 SpA / 252 Spe\nModest Nature\n- Thunderbolt\n- Thunder Wave\n";
    const TEAM_P2: &str = "Tyranitar @ Sitrus Berry\nAbility: Sand Stream\nLevel: 50\nEVs: 252 HP / 4 Atk / 252 SpD\nCareful Nature\n- Tackle\n- Crunch\n";

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
    fn tracker_round_trip_stays_a_sound_superset_of_the_real_battle() {
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
}
