//! Tracker mode: synthesize the "guaranteed" reactions a user should never
//! have to type by hand — anything that follows deterministically from
//! something they *did* type.
//!
//! Four triggers, matching how the real simulator nests these (see
//! `information::information`'s module docs and
//! `simulator::helpers::apply_reaction_self_boost`):
//!
//! - An **`AbilityRevealed`** node (anywhere in the tree — a standalone
//!   `[slot] [ability]` line, an ability named inline on a move line, or one
//!   synthesized by this module itself for Mega Evolution/Trace) gets its
//!   deterministic on-reveal effect appended as a child, exactly the
//!   "nested-reveal convention" the simulator itself uses (Intimidate's
//!   `-1 atk` per opposing active, a weather-setter's field change, …). The
//!   *ability itself* still has to be named by the user for an ordinary
//!   switch-in reveal — that's new information the engine has no way to
//!   guess — only its guaranteed consequence is auto-added.
//! - A **`MoveUsed`** node gets: its move's always-on `self_boost` (Swords
//!   Dance, Nasty Plot, …); and, generically, every 100%-chance,
//!   non-random-choice entry in `MoveData.secondaries`/`self_secondaries` —
//!   these aren't a hand-curated list, they're *already* sitting in the
//!   parsed move dex (Showdown's move data represents a "pure status" move's
//!   entire guaranteed effect — Thunder Wave's Paralysis, Stealth Rock's
//!   hazard, Rain Dance's weather, Leer's -1 Def — as a clean top-level
//!   `status:`/`sideCondition:`/`weather:`/`boosts:` field, and
//!   `state/dex_data.rs::parse_move_entry` already converts every one of
//!   those into a 100%-chance secondary; see this module's design doc for
//!   the research trail). A target already recorded `Missed`/`Immune`/
//!   `Blocked`, or the move recorded `MoveFailed` at all, is skipped.
//! - A **`MegaEvolution`** node gets its mega form's fixed ability (a mega
//!   forme's dex entry always has exactly one ability slot — no hidden
//!   ability — mirroring `state/battle.rs`'s own mega-evolution ability
//!   resolution) appended as a nested `AbilityRevealed`, itself immediately
//!   cascaded through the same ability-reaction table (the outer recursive
//!   walk's children-pass has already finished by the time a node's own kind
//!   is handled, so a newly-synthesized `AbilityRevealed` can't rely on the
//!   recursion to process it — see `ability_revealed_node`).
//! - **Any node** gets a `Faint` sibling synthesized for any
//!   `DamageDealt`/`Healed`/`SetHp` child that lands exactly on 0 HP — the
//!   real simulator always emits `Faint` as an explicit sibling of the
//!   zero-HP event (never implied by it), and Pass 1's own auto-faint
//!   belt-and-braces only covers the `DamageDealt` arm, so this closes the
//!   `Healed`/`SetHp`-to-0 gap and matches real emitted trees either way.
//!
//! # Scope
//!
//! The move-secondary synthesis intentionally skips `slot_condition`
//! payloads (Wish/Future Sight/Doom Desire need extra snapshot data —
//! attacker stats, ability, item at cast time — the tracker doesn't have
//! readily available) — those stay user-typed. The ability table
//! (Intimidate, the four weather/terrain-setter pairs, Intrepid Sword/
//! Dauntless Shield, Download, Trace) is, and will likely remain, hand-
//! curated: unlike moves, an ability's effect lives entirely in unparsed JS
//! with no structured dex equivalent (confirmed against Download/Trace/
//! Frisk/Forewarn) — Frisk/Forewarn are excluded because they reveal
//! genuinely new information the user has to type anyway (revealing it IS
//! the ability's whole purpose). Weather/terrain-setter *abilities* only
//! fire when the field doesn't already have one active — modeling
//! Primordial Sea/Desolate Land blocking a weaker setter, or two setters
//! entering together, is out of scope. Intrepid Sword/Dauntless
//! Shield/one-time abilities are gated on `one_time_ability_used` so a
//! Pokemon that already fired one doesn't get boosted again on a later
//! switch-in. Download only fires when the opposing actives' known Def/SpD
//! bounds make the comparison unambiguous; Trace only fires when exactly one
//! opposing active's ability is already `Known`.

use std::collections::HashMap;

use poke_rust::data::ability::Ability;
use poke_rust::data::pokemon_move::PokemonMove;
use poke_rust::data::species::Species;
use poke_rust::information::information::{EventKind, InformationEvent};
use poke_rust::information::unknowns::{PokemonHP, Unknown, UnknownBattleState, UnknownPokemonState};
use poke_rust::state::battle::{FieldSlot, Player};
use poke_rust::state::dex_data::{MoveData, MoveTarget, PokemonData, Terrain, Weather};

use crate::tracker_parse::opposing_active_slots;

/// Walk `event`'s tree and append guaranteed reactions in place. `belief` is
/// read-only — it reflects the state *before* this turn's lines are applied,
/// which is all the synthesis below needs (none of it depends on anything
/// else this same turn changes first).
pub fn augment_with_guaranteed_effects(
    mut event: InformationEvent,
    belief: &UnknownBattleState,
    move_dex: &HashMap<PokemonMove, MoveData>,
    pokemon_dex: &HashMap<Species, PokemonData>,
) -> InformationEvent {
    event.reactions = event
        .reactions
        .into_iter()
        .map(|r| augment_with_guaranteed_effects(r, belief, move_dex, pokemon_dex))
        .collect();

    match &event.kind {
        EventKind::AbilityRevealed { slot, ability } => {
            event
                .reactions
                .extend(guaranteed_ability_reactions(*slot, ability, belief));
        }
        EventKind::MoveUsed {
            user,
            move_used,
            targets,
        } => {
            if let Some(md) = move_dex.get(move_used)
                && !move_failed_at_all(&event.reactions)
            {
                for (idx, &delta) in md.self_boost.iter().enumerate() {
                    if delta != 0 {
                        event.reactions.push(leaf(EventKind::BoostChanged {
                            target: *user,
                            boost_idx: idx,
                            stages: delta,
                        }));
                    }
                }

                // Field-level payloads (weather/terrain/pseudo-weather/side
                // condition) apply once per move use, regardless of which
                // bucket housed them — see this module's doc comment.
                for sec in md
                    .secondaries
                    .iter()
                    .chain(md.self_secondaries.iter())
                    .filter(|s| s.chance == 100 && s.random_choices.is_empty())
                {
                    synthesize_field_effects(&mut event.reactions, &sec.effect, *user, &md.target);
                }

                // Per-Pokemon payloads (status/volatile/boosts): `secondaries`
                // apply to each connected target, `self_secondaries` to the user.
                for sec in md
                    .secondaries
                    .iter()
                    .filter(|s| s.chance == 100 && s.random_choices.is_empty())
                {
                    for &target in targets {
                        if !target_has_failed(&event.reactions, target) {
                            synthesize_per_mon_effects(&mut event.reactions, &sec.effect, target);
                        }
                    }
                }
                for sec in md
                    .self_secondaries
                    .iter()
                    .filter(|s| s.chance == 100 && s.random_choices.is_empty())
                {
                    if !target_has_failed(&event.reactions, *user) {
                        synthesize_per_mon_effects(&mut event.reactions, &sec.effect, *user);
                    }
                }
            }
        }
        EventKind::MegaEvolution { slot, into } => {
            if let Some(ability) = pokemon_dex.get(into).and_then(|d| d.primary_ability.clone()) {
                event
                    .reactions
                    .push(ability_revealed_node(*slot, ability, belief));
            }
        }
        _ => {}
    }

    synthesize_guaranteed_faints(&mut event.reactions);
    event
}

/// A move-wide failure (the user typed `fail`/`failed` anywhere on the line)
/// blocks every guaranteed consequence of the move — self-boost, field
/// effects, per-target effects alike — not just the specific slot it was
/// recorded against (the exact slot `MoveFailed.slot` names depends on
/// whichever target was "current" when the user typed it — see
/// `tracker_parse.rs`'s "flat nesting" token-stream convention — so this
/// checks for presence, not a specific slot match).
fn move_failed_at_all(reactions: &[InformationEvent]) -> bool {
    reactions
        .iter()
        .any(|r| matches!(r.kind, EventKind::MoveFailed { .. }))
}

/// True if `target` already has a recorded `Missed`/`Immune`/`Blocked` among
/// `reactions` — the move didn't actually connect there, so no guaranteed
/// on-hit effect should be synthesized for it.
fn target_has_failed(reactions: &[InformationEvent], target: FieldSlot) -> bool {
    reactions.iter().any(|r| match &r.kind {
        EventKind::Missed { target: t }
        | EventKind::Immune { target: t }
        | EventKind::Blocked { target: t } => *t == target,
        _ => false,
    })
}

/// Weather/terrain/pseudo-weather/side-condition — global or per-side field
/// state, applied once per move use rather than per target.
fn synthesize_field_effects(
    reactions: &mut Vec<InformationEvent>,
    effect: &poke_rust::state::dex_data::HitEffect,
    user: FieldSlot,
    move_target: &MoveTarget,
) {
    if let Some(weather) = &effect.weather
        && !reactions.iter().any(
            |r| matches!(&r.kind, EventKind::WeatherChanged { weather: Some(w) } if w == weather),
        )
    {
        reactions.push(leaf(EventKind::WeatherChanged {
            weather: Some(weather.clone()),
        }));
    }
    if let Some(terrain) = &effect.terrain
        && !reactions.iter().any(
            |r| matches!(&r.kind, EventKind::TerrainChanged { terrain: Some(t) } if t == terrain),
        )
    {
        reactions.push(leaf(EventKind::TerrainChanged {
            terrain: Some(terrain.clone()),
        }));
    }
    if let Some(pw) = &effect.pseudo_weather
        && !reactions
            .iter()
            .any(|r| matches!(&r.kind, EventKind::PseudoWeatherStart { effect: e } if e == pw))
    {
        reactions.push(leaf(EventKind::PseudoWeatherStart { effect: pw.clone() }));
    }
    if let Some(sc) = &effect.side_condition {
        // `FoeSide` lands on the user's opponent; everything else that can
        // carry a side condition (`AllySide`/`AllyTeam`) lands on the user's
        // own side.
        let side = match move_target {
            MoveTarget::FoeSide => user.player.opponent(),
            _ => user.player,
        };
        if !reactions.iter().any(|r| {
            matches!(&r.kind, EventKind::SideConditionStart { side: s, condition } if *s == side && condition == sc)
        }) {
            reactions.push(leaf(EventKind::SideConditionStart {
                side,
                condition: sc.clone(),
            }));
        }
    }
}

/// Status/volatile/boosts — per-Pokemon payloads, applied to one specific
/// `target` (a connected foe for `secondaries`, the user for
/// `self_secondaries`).
fn synthesize_per_mon_effects(
    reactions: &mut Vec<InformationEvent>,
    effect: &poke_rust::state::dex_data::HitEffect,
    target: FieldSlot,
) {
    if let Some(status) = &effect.status {
        reactions.push(leaf(EventKind::StatusInflicted {
            target,
            status: status.clone(),
        }));
    }
    if let Some(volatile) = &effect.volatile_status {
        reactions.push(leaf(EventKind::VolatileStart {
            target,
            volatile: volatile.clone(),
        }));
    }
    for (idx, &delta) in effect.boosts.iter().enumerate() {
        if delta != 0 {
            reactions.push(leaf(EventKind::BoostChanged {
                target,
                boost_idx: idx,
                stages: delta,
            }));
        }
    }
}

/// Any `DamageDealt`/`Healed`/`SetHp` child landing exactly on 0 HP gets a
/// `Faint` sibling — see this module's doc comment for why this can't be left
/// to Pass 1's belt-and-braces alone.
fn synthesize_guaranteed_faints(reactions: &mut Vec<InformationEvent>) {
    let mut faint_slots: Vec<FieldSlot> = Vec::new();
    for r in reactions.iter() {
        let zeroed = match &r.kind {
            EventKind::DamageDealt { target, new_hp, .. }
            | EventKind::Healed { target, new_hp, .. }
            | EventKind::SetHp { target, new_hp, .. } => is_zero_hp(new_hp).then_some(*target),
            _ => None,
        };
        if let Some(target) = zeroed
            && !faint_slots.contains(&target)
            && !reactions
                .iter()
                .any(|r2| matches!(&r2.kind, EventKind::Faint { slot } if *slot == target))
        {
            faint_slots.push(target);
        }
    }
    for slot in faint_slots {
        reactions.push(leaf(EventKind::Faint { slot }));
    }
}

fn is_zero_hp(hp: &PokemonHP) -> bool {
    matches!(hp, PokemonHP::Number(0) | PokemonHP::Percent(0))
}

fn mon_at(belief: &UnknownBattleState, slot: FieldSlot) -> Option<&UnknownPokemonState> {
    let mons = match slot.player {
        Player::P1 => &belief.p1_active_mons,
        Player::P2 => &belief.p2_active_mons,
    };
    mons.get(slot.slot_index as usize)
}

/// Build an `AbilityRevealed` node with its own cascade already populated —
/// use this (not a bare `leaf(...)`) whenever this module synthesizes a NEW
/// ability reveal (Mega Evolution, Trace) rather than translating one the
/// user typed: the outer recursive walk only processes a node's *existing*
/// children before handling its own kind, so a freshly-appended
/// `AbilityRevealed` would never otherwise get its cascade applied.
fn ability_revealed_node(slot: FieldSlot, ability: Ability, belief: &UnknownBattleState) -> InformationEvent {
    let reactions = guaranteed_ability_reactions(slot, &ability, belief);
    InformationEvent {
        kind: EventKind::AbilityRevealed { slot, ability },
        reactions,
    }
}

fn guaranteed_ability_reactions(
    slot: FieldSlot,
    ability: &Ability,
    belief: &UnknownBattleState,
) -> Vec<InformationEvent> {
    match ability {
        Ability::Intimidate => opposing_active_slots(belief, slot)
            .into_iter()
            .map(|target| {
                leaf(EventKind::BoostChanged {
                    target,
                    boost_idx: 0,
                    stages: -1,
                })
            })
            .collect(),
        Ability::Drizzle if belief.weather.is_none() => vec![weather_event(Weather::Rain)],
        Ability::Drought if belief.weather.is_none() => vec![weather_event(Weather::Sun)],
        Ability::SandStream if belief.weather.is_none() => vec![weather_event(Weather::Sandstorm)],
        Ability::SnowWarning if belief.weather.is_none() => vec![weather_event(Weather::Snow)],
        Ability::ElectricSurge if belief.terrain.is_none() => {
            vec![terrain_event(Terrain::ElectricTerrain)]
        }
        Ability::GrassySurge if belief.terrain.is_none() => {
            vec![terrain_event(Terrain::GrassyTerrain)]
        }
        Ability::MistySurge if belief.terrain.is_none() => {
            vec![terrain_event(Terrain::MistyTerrain)]
        }
        Ability::PsychicSurge if belief.terrain.is_none() => {
            vec![terrain_event(Terrain::PsychicTerrain)]
        }
        Ability::IntrepidSword
            if !mon_at(belief, slot).is_some_and(|m| m.one_time_ability_used) =>
        {
            vec![leaf(EventKind::BoostChanged {
                target: slot,
                boost_idx: 0,
                stages: 1,
            })]
        }
        Ability::DauntlessShield
            if !mon_at(belief, slot).is_some_and(|m| m.one_time_ability_used) =>
        {
            vec![leaf(EventKind::BoostChanged {
                target: slot,
                boost_idx: 1,
                stages: 1,
            })]
        }
        Ability::Download => download_reaction(slot, belief),
        Ability::Trace => trace_reaction(slot, belief),
        _ => Vec::new(),
    }
}

/// Download compares the SUM of opposing actives' Def vs SpD (mirroring the
/// real ability's `for target of pokemon.foes()` loop) and boosts SpA when
/// Def is the (weaker-to-hit) higher stat, Atk when SpD is — but only when
/// the belief's current bounds make the comparison unambiguous regardless of
/// where the true values land; otherwise this is left for the user to type.
fn download_reaction(slot: FieldSlot, belief: &UnknownBattleState) -> Vec<InformationEvent> {
    let opponents = opposing_active_slots(belief, slot);
    let (mut min_def, mut max_def, mut min_spd, mut max_spd) = (0u32, 0u32, 0u32, 0u32);
    for opp in &opponents {
        let Some(mon) = mon_at(belief, *opp) else {
            return Vec::new();
        };
        min_def += mon.min_stats[2] as u32;
        max_def += mon.max_stats[2] as u32;
        min_spd += mon.min_stats[4] as u32;
        max_spd += mon.max_stats[4] as u32;
    }
    if min_def >= max_spd {
        vec![leaf(EventKind::BoostChanged {
            target: slot,
            boost_idx: 2, // SpA
            stages: 1,
        })]
    } else if max_def < min_spd {
        vec![leaf(EventKind::BoostChanged {
            target: slot,
            boost_idx: 0, // Atk
            stages: 1,
        })]
    } else {
        Vec::new()
    }
}

/// Trace copies an opposing active's ability — only synthesizable when
/// exactly one opposing active's ability is already `Known` (otherwise which
/// one the real game copied is new information the user has to type).
fn trace_reaction(slot: FieldSlot, belief: &UnknownBattleState) -> Vec<InformationEvent> {
    let known: Vec<Ability> = opposing_active_slots(belief, slot)
        .into_iter()
        .filter_map(|opp| mon_at(belief, opp))
        .filter_map(|mon| match &mon.possible_abilities {
            Unknown::Known(a) => Some(a.clone()),
            _ => None,
        })
        .collect();
    match known.as_slice() {
        [ability] => vec![ability_revealed_node(slot, ability.clone(), belief)],
        _ => Vec::new(),
    }
}

fn weather_event(weather: Weather) -> InformationEvent {
    leaf(EventKind::WeatherChanged {
        weather: Some(weather),
    })
}

fn terrain_event(terrain: Terrain) -> InformationEvent {
    leaf(EventKind::TerrainChanged {
        terrain: Some(terrain),
    })
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
    use crate::tracker_parse::{TrackerLine, parse_tracker_text};
    use poke_rust::information::inference::{InferenceConfig, apply_information};
    use poke_rust::information::unknowns::{PokemonHP, UnknownMatchState, UnknownPokemonState};
    use poke_rust::state::battle::Player;
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

    fn make_active(species: Species, hp: PokemonHP) -> UnknownPokemonState {
        let mut mon = UnknownPokemonState::from_opponent_species(species, pokemon_dex(), 50);
        mon.hp = hp;
        mon
    }

    fn test_belief() -> UnknownBattleState {
        UnknownBattleState {
            active_per_side: 1,
            back_mons_per_side: 0,
            p1_active_mons: vec![make_active(Species::Pikachu, PokemonHP::Number(100))],
            p2_active_mons: vec![make_active(Species::Garchomp, PokemonHP::Percent(100))],
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

    /// Parse one line, augment it, and return the resulting event.
    fn parse_and_augment(text: &str, belief: &UnknownBattleState) -> InformationEvent {
        let lines = parse_tracker_text(text, belief, move_dex(), pokemon_dex()).unwrap();
        let TrackerLine::Event(ev) = lines.into_iter().next().unwrap() else {
            panic!("expected an event line")
        };
        augment_with_guaranteed_effects(ev, belief, move_dex(), pokemon_dex())
    }

    #[test]
    fn thunder_wave_synthesizes_guaranteed_paralysis() {
        let belief = test_belief();
        let augmented = parse_and_augment("p1 thunderwave o1", &belief);
        assert!(augmented.reactions.iter().any(|r| matches!(
            &r.kind,
            EventKind::StatusInflicted { target, status: poke_rust::state::dex_data::Status::Paralysis } if *target == o1()
        )));
    }

    #[test]
    fn leer_synthesizes_foe_defense_drop() {
        let belief = test_belief();
        let augmented = parse_and_augment("p1 leer o1", &belief);
        assert!(augmented.reactions.iter().any(|r| matches!(
            &r.kind,
            EventKind::BoostChanged { target, boost_idx: 1, stages: -1 } if *target == o1()
        )));
    }

    #[test]
    fn rain_dance_synthesizes_global_weather_with_no_target_token() {
        let belief = test_belief();
        let augmented = parse_and_augment("p1 raindance", &belief);
        assert!(augmented.reactions.iter().any(|r| matches!(
            &r.kind,
            EventKind::WeatherChanged { weather: Some(Weather::Rain) }
        )));
    }

    #[test]
    fn stealth_rock_synthesizes_side_condition_on_foe_side_with_no_target_token() {
        let belief = test_belief();
        let augmented = parse_and_augment("p1 stealthrock", &belief);
        assert!(augmented.reactions.iter().any(|r| matches!(
            &r.kind,
            EventKind::SideConditionStart { side: Player::P2, condition: poke_rust::state::dex_data::SideCondition::StealthRock }
        )));
    }

    #[test]
    fn missed_target_does_not_get_guaranteed_status() {
        let belief = test_belief();
        let augmented = parse_and_augment("p1 thunderwave o1 miss", &belief);
        assert!(!augmented.reactions.iter().any(|r| matches!(
            &r.kind,
            EventKind::StatusInflicted { .. }
        )));
    }

    #[test]
    fn mega_evolution_reveals_fixed_ability_with_its_own_cascade() {
        // Charizard-Mega-Y's fixed ability is Drought, which itself
        // guarantees a Sun weather change — the cascade must appear nested
        // under the synthesized AbilityRevealed, not as a sibling of it.
        let belief = test_belief();
        let event = InformationEvent {
            kind: EventKind::MegaEvolution {
                slot: p1(),
                into: Species::CharizardMegaY,
            },
            reactions: Vec::new(),
        };
        let augmented = augment_with_guaranteed_effects(event, &belief, move_dex(), pokemon_dex());
        let ability_node = augmented
            .reactions
            .iter()
            .find(|r| matches!(&r.kind, EventKind::AbilityRevealed { ability: Ability::Drought, .. }))
            .expect("expected a synthesized Drought reveal");
        assert!(ability_node.reactions.iter().any(|r| matches!(
            &r.kind,
            EventKind::WeatherChanged { weather: Some(Weather::Sun) }
        )));
    }

    #[test]
    fn zero_hp_synthesizes_faint_and_applies_through_inference() {
        let belief = test_belief();
        let lines = parse_tracker_text(
            "p1 thunderbolt o1 0%\nendofturn",
            &belief,
            move_dex(),
            pokemon_dex(),
        )
        .unwrap();

        let mut events = Vec::new();
        for line in lines {
            match line {
                TrackerLine::Event(ev) => {
                    events.push(augment_with_guaranteed_effects(ev, &belief, move_dex(), pokemon_dex()))
                }
                TrackerLine::EndOfTurn => events.push(leaf(EventKind::EndOfTurn)),
            }
        }

        // The synthesized Faint sibling should already be present pre-inference.
        let move_event = events
            .iter()
            .find(|e| matches!(e.kind, EventKind::MoveUsed { .. }))
            .unwrap();
        assert!(move_event
            .reactions
            .iter()
            .any(|r| matches!(&r.kind, EventKind::Faint { slot } if *slot == o1())));

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
        assert!(next.p2_active_mons[0].fainted);
    }
}
