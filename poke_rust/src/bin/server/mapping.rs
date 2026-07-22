//! Engine state → DTO conversion. All display strings are produced here so the
//! frontend never sees raw enum debug output it didn't ask for.

use std::collections::HashSet;

use crate::dto::*;
use poke_rust::data::item::Item;
use poke_rust::information::describe::{
    describe_clause, describe_move_slot_union, describe_unknown, describe_unknown_item_union,
    describe_unknown_union,
};
use poke_rust::information::information::{CantReason, EventKind, InformationEvent, SwitchState};
use poke_rust::information::unknowns::{
    PokemonHP, Unknown, UnknownBattleState, UnknownMatchState, UnknownPokemonState,
};
use poke_rust::state::battle::{
    BattleCommand, BattleState, FieldSlot, MatchState, Player, TeamPreviewState,
};
use poke_rust::state::dex_data::{SideCondition, SlotCondition, Status, VolatileStatus};
use poke_rust::state::pokemon::{PokemonState, VolatileStatusState};
use poke_rust::user::{back_mon_name, battle_command_description, humanize_identifier, move_name};

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

/// `is_own_side`: `true` when rendering the mon for its OWNING player (they always
/// see their own true identity, disguise or not — a player obviously knows their
/// own Zoroark is Zoroark), `false` when rendering it for the OPPONENT (who only
/// ever sees the physically-displayed appearance, i.e. the Illusion disguise when
/// one is active). Getting this backwards was a real bug: `pokemon_view` used to
/// apply the disguise unconditionally, so a player's own disguised Zoroark showed
/// up to THEM as the disguise species instead of Zoroark.
pub fn pokemon_view(mon: &PokemonState, is_own_side: bool) -> PokemonView {
    PokemonView {
        mon_id: mon.mon_id,
        // `mon.types` still reflects the TRUE species since it drives damage calc;
        // a fully faithful disguised-type display would need a dex lookup this
        // function doesn't have, so it's left as a known gap.
        species: if is_own_side {
            mon.species.to_string()
        } else {
            mon.illusion_disguise
                .as_ref()
                .map(|s| s.to_string())
                .unwrap_or_else(|| mon.species.to_string())
        },
        level: mon.level,
        gender: format!("{:?}", mon.gender),
        types: mon.types.iter().map(|t| format!("{:?}", t)).collect(),
        hp: ObservedHpDto {
            exact: Some(mon.hp),
            percent: None,
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
/// mode, so those stay ground truth — with one exception: `ChoiceLock` (see
/// `mon_volatiles`'s param doc below) is re-derived from the belief, not copied.
fn mask_pokemon_view(
    mut view: PokemonView,
    unk: &UnknownPokemonState,
    legal_items: Option<&HashSet<Item>>,
    // Ground-truth volatiles for this same mon, re-filtered here rather than trusted
    // from `view.volatiles` (which `pokemon_view` already populated from the same
    // source) — see the `ChoiceLock` handling below. `VolatileDto` is a display
    // string with no variant tag, so the filter has to happen on the raw engine type.
    mon_volatiles: &[VolatileStatusState],
) -> PokemonView {
    // A live Zoroark hypothesis widens every hidden-attribute display (but never
    // species/typing — those stay the shown/original identity; see this function's
    // doc comment) to the union of the primary and the hypothesis. `hyp` is `None`
    // for the overwhelming majority of mons (no suspected disguise), in which case
    // every `_union` call below is identical to its non-union counterpart.
    let hyp = unk.possible_illusion_state.as_deref();

    view.nature = describe_unknown_union(&unk.possible_natures, hyp.map(|h| &h.possible_natures));
    view.stats = merge_stats_min(&unk.min_stats, hyp.map(|h| &h.min_stats));
    view.stats_max = merge_stats_max(&unk.max_stats, hyp.map(|h| &h.max_stats));
    view.evs = merge_evs_min(&unk.min_evs, hyp.map(|h| &h.min_evs));
    view.evs_max = merge_evs_max(&unk.max_evs, hyp.map(|h| &h.max_evs));
    // A real player only ever sees the opponent's HP as a rounded percent, never the
    // exact value — replace the true `mon.hp` set by `pokemon_view` with the belief's
    // own observed representation (never compute a fake-precise number back out of a
    // percent; that reintroduces the exact precision a real player never has).
    view.hp = observed_hp_dto(&unk.hp);
    // Typing/species always stay the shown/original identity — a live hypothesis
    // never widens these (see `possible_illusion_state`'s doc comment: species is
    // always `Known` for the primary, and typing is public dex knowledge for
    // whichever species is actually displayed).
    view.types = match &unk.possible_types {
        Unknown::Known(types) => types.iter().map(|t| format!("{:?}", t)).collect(),
        _ => Vec::new(),
    };
    view.item = match (&unk.item, hyp) {
        (Unknown::Known(Item::None), None) => None,
        (primary, _) => Some(describe_unknown_item_union(
            primary,
            hyp.map(|h| &h.item),
            legal_items,
        )),
    };
    // TODO.md: Choice Lock is the one volatile in this codebase that's a SILENT
    // consequence of a still-hidden held item — no in-game message announces "X is
    // now locked into its move" the way Substitute/Leech Seed/Encore/Taunt/etc. all
    // get their own announced message (see this function's doc comment on why every
    // OTHER volatile is safe to copy from ground truth). A real opponent can only be
    // sure of it once the item itself is confirmed as a Choice item — showing it
    // earlier leaks information the belief hasn't actually earned. Re-derive
    // `view.volatiles` from the raw ground truth here (rather than trust what
    // `pokemon_view` already put there) so this is the one place that can drop it.
    let item_is_confirmed_choice = |it: &Unknown<Item>| {
        matches!(
            it,
            Unknown::Known(Item::ChoiceBand | Item::ChoiceSpecs | Item::ChoiceScarf)
        )
    };
    let choice_lock_provable = item_is_confirmed_choice(&unk.item)
        || hyp.is_some_and(|h| item_is_confirmed_choice(&h.item));
    view.volatiles = mon_volatiles
        .iter()
        .filter(|v| {
            choice_lock_provable
                || !matches!(
                    v,
                    VolatileStatusState::TurnStatus(VolatileStatus::ChoiceLock(_), _)
                )
        })
        .map(volatile_dto)
        .collect();
    view.ability =
        describe_unknown_union(&unk.possible_abilities, hyp.map(|h| &h.possible_abilities));
    view.moves = (0..4)
        .map(|i| {
            Some(MoveViewDto {
                name: describe_move_slot_union(
                    unk.known_moves[i].clone(),
                    hyp.and_then(|h| h.known_moves[i].clone()),
                ),
                pp: unk.move_pp[i].max(0) as u8,
                max_pp: unk.max_pp[i].max(0) as u8,
            })
        })
        .collect();
    // A pre-reveal Tera type is genuinely secret in a real battle too (the Tera
    // Orb icon only shows it once activated) — mask it until `is_tera` flips true.
    if !view.is_tera {
        view.tera_type =
            describe_unknown_union(&unk.possible_tera_type, hyp.map(|h| &h.possible_tera_type));
    }
    view.is_illusion_suspected = hyp.is_some();
    view
}

/// Element-wise minimum-of-minimums across a primary stat array and a live
/// hypothesis's own array — the union's lower bound is the lower of the two
/// hypotheses' lower bounds (whichever identity is real, the true value could be
/// as low as the more permissive of the two).
fn merge_stats_min(primary: &[u16; 6], hyp: Option<&[u16; 6]>) -> [u16; 6] {
    let mut out = *primary;
    if let Some(h) = hyp {
        for i in 0..6 {
            out[i] = out[i].min(h[i]);
        }
    }
    out
}

/// Symmetric upper-bound companion to `merge_stats_min`.
fn merge_stats_max(primary: &[u16; 6], hyp: Option<&[u16; 6]>) -> [u16; 6] {
    let mut out = *primary;
    if let Some(h) = hyp {
        for i in 0..6 {
            out[i] = out[i].max(h[i]);
        }
    }
    out
}

fn merge_evs_min(primary: &[u8; 6], hyp: Option<&[u8; 6]>) -> [u8; 6] {
    let mut out = *primary;
    if let Some(h) = hyp {
        for i in 0..6 {
            out[i] = out[i].min(h[i]);
        }
    }
    out
}

fn merge_evs_max(primary: &[u8; 6], hyp: Option<&[u8; 6]>) -> [u8; 6] {
    let mut out = *primary;
    if let Some(h) = hyp {
        for i in 0..6 {
            out[i] = out[i].max(h[i]);
        }
    }
    out
}

/// Build a `PokemonView` for a benched Pokémon from the belief alone (no reliable
/// concrete `PokemonState` pairing exists for bench mons — the inference engine's
/// own bench bookkeeping doesn't preserve list order against `BattleState`'s). Boosts
/// and volatiles are exactly `[0;7]`/empty for any benched mon (both reset on
/// switch-out), so this is not an approximation for those two fields; HP is the
/// belief's own observed representation (percent, same as an active opponent mon).
///
/// `mon_id` prefers the belief's own `possible_mon_id` (narrowed to `Known` once the
/// party-order slot is pinned down); when it's still ambiguous, falls back to
/// `fallback_id` — a caller-supplied id that must be unique across the whole side's
/// bench render for this call (see `side_view`). Without this, every unresolved bench
/// mon rendered `mon_id: 0` and collided on the frontend's `mon_id`-keyed rows.
fn bench_pokemon_view_from_belief(
    unk: &UnknownPokemonState,
    fallback_id: u8,
    legal_items: Option<&HashSet<Item>>,
) -> PokemonView {
    let mon_id = match unk.possible_mon_id {
        Unknown::Known(id) => id,
        _ => fallback_id,
    };
    let hyp = unk.possible_illusion_state.as_deref();
    let mut view = PokemonView {
        mon_id,
        species: describe_unknown(&unk.possible_species),
        level: unk.level,
        gender: describe_unknown(&unk.possible_genders),
        types: match &unk.possible_types {
            Unknown::Known(types) => types.iter().map(|t| format!("{:?}", t)).collect(),
            _ => Vec::new(),
        },
        hp: observed_hp_dto(&unk.hp),
        fainted: unk.fainted,
        status: unk.status.as_ref().map(status_dto),
        volatiles: unk.volatiles.iter().map(volatile_dto).collect(),
        stats: merge_stats_min(&unk.min_stats, hyp.map(|h| &h.min_stats)),
        stats_max: merge_stats_max(&unk.max_stats, hyp.map(|h| &h.max_stats)),
        boosts: [0; 7],
        nature: describe_unknown_union(&unk.possible_natures, hyp.map(|h| &h.possible_natures)),
        evs: merge_evs_min(&unk.min_evs, hyp.map(|h| &h.min_evs)),
        evs_max: merge_evs_max(&unk.max_evs, hyp.map(|h| &h.max_evs)),
        item: None,
        ability: describe_unknown_union(
            &unk.possible_abilities,
            hyp.map(|h| &h.possible_abilities),
        ),
        moves: Vec::new(),
        is_tera: unk.is_tera,
        tera_type: describe_unknown_union(
            &unk.possible_tera_type,
            hyp.map(|h| &h.possible_tera_type),
        ),
        is_mega: unk.is_mega,
        is_illusion_suspected: hyp.is_some(),
    };
    view.item = match (&unk.item, hyp) {
        (Unknown::Known(Item::None), None) => None,
        (primary, _) => Some(describe_unknown_item_union(
            primary,
            hyp.map(|h| &h.item),
            legal_items,
        )),
    };
    view.moves = (0..4)
        .map(|i| {
            Some(MoveViewDto {
                name: describe_move_slot_union(
                    unk.known_moves[i].clone(),
                    hyp.and_then(|h| h.known_moves[i].clone()),
                ),
                pp: unk.move_pp[i].max(0) as u8,
                max_pp: unk.max_pp[i].max(0) as u8,
            })
        })
        .collect();
    view
}

fn named_turns(name: String, turns: Option<u8>) -> NamedTurnsDto {
    NamedTurnsDto { name, turns, turns_max: None }
}

/// Derive a display-safe range from an `Unknown<u8>` duration/turn-count.
/// `Known(n)` is an exact `n..=n`. `Possibly(candidates)` — the common case
/// under fog-of-war, e.g. weather's `[5, 8]` when the setter's extension rock
/// hasn't been revealed yet — becomes the min/max of the candidate set: a
/// real observer can't narrow it any further than that, so this must never
/// collapse to a single value while more than one is still possible. `Not(_)`
/// holds EXCLUDED values, not a candidate set, so there's no bounded range to
/// report from it — falls back to `None`, same as this field's behavior
/// before ranges existed.
fn turn_range(turns: &Unknown<u8>) -> Option<(u8, u8)> {
    match turns {
        Unknown::Known(n) => Some((*n, *n)),
        Unknown::Possibly(candidates) => {
            let min = candidates.iter().min().copied()?;
            let max = candidates.iter().max().copied()?;
            Some((min, max))
        }
        Unknown::Not(_) => None,
    }
}

/// Belief-driven counterpart to `named_turns`: takes an already-derived
/// `(min, max)` range (from `turn_range`) rather than a pre-collapsed
/// `Option<u8>`, so a `Possibly` range (e.g. "5-8 turns of weather, depending
/// on an unrevealed rock") comes through as a range instead of silently going
/// blank — see `NamedTurnsDto`'s doc comment. `turns_max` is omitted when the
/// range has already collapsed to a single value.
fn named_turns_ranged(name: String, range: Option<(u8, u8)>) -> NamedTurnsDto {
    match range {
        Some((min, max)) if min == max => NamedTurnsDto { name, turns: Some(min), turns_max: None },
        Some((min, max)) => NamedTurnsDto { name, turns: Some(min), turns_max: Some(max) },
        None => NamedTurnsDto { name, turns: None, turns_max: None },
    }
}

// ── Tracker mode: rendering a `BattleView` from the belief alone ─────────────
//
// Tracker mode has no concrete `MatchState`/`BattleState` to pair against — the
// belief IS the state (see `bin/server/tracker.rs`'s module doc). These mirror
// `battle_view`/`side_view`/`field_view` above but read every field, including
// the tracker viewer's own (always fully `Known`) side, through the same
// fog-describing helpers ground truth never needed before.

/// Build a `PokemonView` for an ACTIVE Pokemon straight from the belief alone.
/// Differs from `bench_pokemon_view_from_belief` only in boosts/volatiles: an
/// active mon's stage boosts and volatile statuses are live battle state, not
/// the always-reset-on-switch-out `[0;7]`/empty a benched mon gets.
pub fn pokemon_view_from_belief(
    unk: &UnknownPokemonState,
    fallback_id: u8,
    legal_items: Option<&HashSet<Item>>,
) -> PokemonView {
    let mut view = bench_pokemon_view_from_belief(unk, fallback_id, legal_items);
    view.boosts = unk.boosts;
    view.volatiles = unk.volatiles.iter().map(volatile_dto).collect();
    view
}

/// Tracker-mode counterpart to `battle_view`. The tracker viewer is always
/// physical `Player::P1` — there is only ever one real perspective in tracker
/// mode, so unlike `battle_view` there is no `perspective` parameter: P1's side
/// renders as exact values because its `UnknownPokemonState` entries are
/// themselves fully `Known` by construction (seeded from the viewer's real
/// team), and P2's side always renders through the masking-describe helpers.
pub fn battle_view_from_belief(
    belief: &UnknownBattleState,
    active_per_side: u8,
    brought_per_side: u8,
    legal_items: Option<&HashSet<Item>>,
) -> BattleView {
    BattleView {
        phase: PhaseDto::Normal,
        turn_number: belief.turn_number,
        active_per_side,
        brought_per_side,
        preview: None,
        p1: Some(side_view_from_belief(belief, Player::P1, legal_items)),
        p2: Some(side_view_from_belief(belief, Player::P2, legal_items)),
        field: Some(field_view_from_belief(belief)),
        self_switch: None,
        winner: None,
        belief: Some(BeliefView {
            clauses: belief
                .predicates
                .iter()
                .map(|clause| describe_clause(clause, belief))
                .collect(),
        }),
    }
}

fn side_view_from_belief(
    belief: &UnknownBattleState,
    player: Player,
    legal_items: Option<&HashSet<Item>>,
) -> SideView {
    #[allow(clippy::type_complexity)]
    let (active, known_back, possible_back, fainted, can_tera, can_mega, conditions, condition_turns, slot_conditions): (
        &Vec<UnknownPokemonState>,
        &Vec<UnknownPokemonState>,
        &Vec<UnknownPokemonState>,
        &Vec<UnknownPokemonState>,
        bool,
        bool,
        &Vec<SideCondition>,
        &Vec<Unknown<u8>>,
        &Vec<Vec<SlotCondition>>,
    ) = match player {
        Player::P1 => (
            &belief.p1_active_mons,
            &belief.p1_known_back_mons,
            &belief.p1_possible_back_mons,
            &belief.p1_fainted_mons,
            belief.p1_has_tera,
            belief.p1_has_mega,
            &belief.p1_side_conditions,
            &belief.p1_side_condition_turns,
            &belief.p1_slot_conditions,
        ),
        Player::P2 => (
            &belief.p2_active_mons,
            &belief.p2_known_back_mons,
            &belief.p2_possible_back_mons,
            &belief.p2_fainted_mons,
            belief.p2_has_tera,
            belief.p2_has_mega,
            &belief.p2_side_conditions,
            &belief.p2_side_condition_turns,
            &belief.p2_slot_conditions,
        ),
    };

    SideView {
        active: active
            .iter()
            .enumerate()
            // Base 50+: clear of real party-order ids (0..=5) and of the
            // 100+/150+/200+ bases the bench/possible/fainted buckets below use
            // (see `bench_pokemon_view_from_belief`'s doc comment).
            .map(|(i, unk)| pokemon_view_from_belief(unk, 50 + i as u8, legal_items))
            .collect(),
        back: known_back
            .iter()
            .enumerate()
            .map(|(i, unk)| bench_pokemon_view_from_belief(unk, 100 + i as u8, legal_items))
            .collect(),
        possible_back: possible_back
            .iter()
            .enumerate()
            .map(|(i, unk)| bench_pokemon_view_from_belief(unk, 150 + i as u8, legal_items))
            .collect(),
        fainted: fainted
            .iter()
            .enumerate()
            .map(|(i, unk)| bench_pokemon_view_from_belief(unk, 200 + i as u8, legal_items))
            .collect(),
        can_tera,
        can_mega,
        side_conditions: conditions
            .iter()
            .zip(condition_turns.iter())
            .map(|(c, t)| named_turns_ranged(side_condition_name(c), turn_range(t)))
            .collect(),
        slot_conditions: slot_conditions
            .iter()
            .map(|conds| conds.iter().map(slot_condition_name).collect())
            .collect(),
    }
}

fn field_view_from_belief(belief: &UnknownBattleState) -> FieldView {
    FieldView {
        weather: belief.weather.as_ref().map(|w| {
            named_turns_ranged(
                humanize_identifier(format!("{:?}", w)),
                belief.weather_turns.as_ref().and_then(turn_range),
            )
        }),
        terrain: belief.terrain.as_ref().map(|t| {
            named_turns_ranged(
                humanize_identifier(format!("{:?}", t)),
                belief.terrain_turns.as_ref().and_then(turn_range),
            )
        }),
        pseudo_weathers: belief
            .pseudo_weathers
            .iter()
            .zip(belief.pseudo_weather_turns.iter())
            .map(|(pw, t)| named_turns_ranged(humanize_identifier(format!("{:?}", pw)), turn_range(t)))
            .collect(),
    }
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

/// `belief` is always the fog state *for `perspective`* — but its `p1_*`/`p2_*`
/// fields are **physically bound** to true Player::P1/P2 identity, exactly like
/// ground truth (`UnknownMatchState::team_preview_open_sheet_from_perspective`'s
/// `my_team`/`opponent_team` fog levels land in the physically correct bucket, not
/// a "viewer's own" bucket — see `into_battle_state`'s doc comment). So masking a
/// physical `player`'s side reads that SAME `player`'s belief fields
/// (`belief.p1_*` for `player == P1`, `belief.p2_*` for `player == P2`) — the same
/// `player`-keyed match already used to read ground truth above — whenever
/// `player != perspective`.
fn side_view(
    state: &BattleState,
    player: Player,
    belief: Option<&UnknownMatchState>,
    perspective: Player,
    legal_items: Option<&HashSet<Item>>,
) -> SideView {
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

    // Only the non-viewer side is ever masked — the belief's own viewer sees their
    // team fully known. `active` is zipped by index with the belief's active mons
    // (both stay in lockstep, actives-first, throughout the battle); known/possible
    // back mons are rendered straight from the belief alone (see
    // `bench_pokemon_view_from_belief`'s doc comment for why no concrete pairing is
    // attempted there).
    let fog = if player != perspective {
        belief_battle_state(belief)
    } else {
        None
    };

    let (active_views, back_views, possible_back_views, fainted_views) = match fog {
        Some(fog) => {
            // Belief fields are physically bound — read the SAME `player` this
            // function is rendering, not a constant `p2_*` (see this function's doc
            // comment).
            let (fog_active, fog_known_back, fog_possible_back, fog_fainted) = match player {
                Player::P1 => (
                    &fog.p1_active_mons,
                    &fog.p1_known_back_mons,
                    &fog.p1_possible_back_mons,
                    &fog.p1_fainted_mons,
                ),
                Player::P2 => (
                    &fog.p2_active_mons,
                    &fog.p2_known_back_mons,
                    &fog.p2_possible_back_mons,
                    &fog.p2_fainted_mons,
                ),
            };
            let active_views: Vec<PokemonView> = active
                .iter()
                .enumerate()
                .map(|(i, mon)| {
                    let base = pokemon_view(mon, false); // fog is Some only when player != perspective
                    match fog_active.get(i) {
                        Some(unk) => mask_pokemon_view(base, unk, legal_items, &mon.volatiles),
                        None => base,
                    }
                })
                .collect();
            // Fallback ids for mons whose `possible_mon_id` hasn't narrowed to `Known`
            // yet: real party-order ids only ever range 0..=5, so offsetting each
            // section's fallback base well above that (and apart from each other)
            // guarantees no two bench rows ever collide on `mon_id` — see
            // `bench_pokemon_view_from_belief`'s doc comment.
            let back_views: Vec<PokemonView> = fog_known_back
                .iter()
                .enumerate()
                .map(|(i, unk)| bench_pokemon_view_from_belief(unk, 100 + i as u8, legal_items))
                .collect();
            let possible_back_views: Vec<PokemonView> = fog_possible_back
                .iter()
                .enumerate()
                .map(|(i, unk)| bench_pokemon_view_from_belief(unk, 150 + i as u8, legal_items))
                .collect();
            // Fallback id base 200+: apart from the 100+/150+ bases above so a
            // fainted-mon row can never collide with a known/possible-back row.
            let fainted_views: Vec<PokemonView> = fog_fainted
                .iter()
                .enumerate()
                .map(|(i, unk)| bench_pokemon_view_from_belief(unk, 200 + i as u8, legal_items))
                .collect();
            (active_views, back_views, possible_back_views, fainted_views)
        }
        // fog is None only when player == perspective — this IS the viewer's own side.
        None => (
            active.iter().map(|m| pokemon_view(m, true)).collect(),
            back.iter().map(|m| pokemon_view(m, true)).collect(),
            Vec::new(),
            Vec::new(),
        ),
    };

    SideView {
        active: active_views,
        back: back_views,
        possible_back: possible_back_views,
        fainted: fainted_views,
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

fn preview_view(
    preview: &TeamPreviewState,
    belief: Option<&UnknownMatchState>,
    perspective: Player,
    legal_items: Option<&HashSet<Item>>,
) -> PreviewView {
    // Mirrors `side_view`: only the non-viewer physical side is ever masked, zipped
    // by index with the belief's team-preview mon list (both built from the same
    // species list in the same order — see `team_preview_open_sheet_from_perspective`).
    // The belief's `p1_mons`/`p2_mons` are physically bound (see `side_view`'s doc
    // comment) — mask physical p1 against `belief.p1_mons`, physical p2 against
    // `belief.p2_mons`, never a constant side.
    let (fog_p1_mons, fog_p2_mons): (
        Option<&[UnknownPokemonState]>,
        Option<&[UnknownPokemonState]>,
    ) = match belief {
        Some(UnknownMatchState::TeamPreview(fog)) => (Some(&fog.p1_mons), Some(&fog.p2_mons)),
        _ => (None, None),
    };
    let mask_side = |mons: &[PokemonState],
                     is_own_side: bool,
                     fog_mons: Option<&[UnknownPokemonState]>|
     -> Vec<PokemonView> {
        mons.iter()
            .enumerate()
            .map(|(i, mon)| {
                let base = pokemon_view(mon, is_own_side);
                if is_own_side {
                    return base;
                }
                match fog_mons.and_then(|f| f.get(i)) {
                    Some(unk) => mask_pokemon_view(base, unk, legal_items, &mon.volatiles),
                    None => base,
                }
            })
            .collect()
    };

    PreviewView {
        active_per_side: preview.active_per_side,
        brought_per_side: preview.brought_per_side,
        p1_mons: mask_side(&preview.p1_mons, perspective == Player::P1, fog_p1_mons),
        p2_mons: mask_side(&preview.p2_mons, perspective == Player::P2, fog_p2_mons),
    }
}

/// Build a `BattleView` from `perspective`'s point of view: `belief` must be the fog
/// state tracked *for that perspective* (session.rs holds one belief per physical
/// player — pass the matching one). `state`/`active_per_side`/`brought_per_side` are
/// ground truth and don't depend on perspective; only masking does.
pub fn battle_view(
    state: &MatchState,
    active_per_side: u8,
    brought_per_side: u8,
    belief: Option<&UnknownMatchState>,
    perspective: Player,
    legal_items: Option<&HashSet<Item>>,
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
            view.preview = Some(preview_view(preview, belief, perspective, legal_items));
        }
        MatchState::BattleState(battle) => {
            view.turn_number = battle.turn_number;
            view.p1 = Some(side_view(
                battle,
                Player::P1,
                belief,
                perspective,
                legal_items,
            ));
            view.p2 = Some(side_view(
                battle,
                Player::P2,
                belief,
                perspective,
                legal_items,
            ));
            // Field-level durations (weather/terrain/side conditions) must be
            // masked exactly like per-mon data is: under any fog-of-war mode,
            // read them through the belief (as a sound RANGE — see
            // `field_view_from_belief`/`turn_range`), not straight off ground
            // truth. `belief_battle_state` returns `None` specifically for
            // Perfect Information (or a not-yet-Battle-phase belief), where
            // showing ground truth is correct rather than a leak.
            view.field = Some(match belief_battle_state(belief) {
                Some(fog) => field_view_from_belief(fog),
                None => field_view(battle),
            });
            view.self_switch = battle
                .self_switch_pending
                .map(|(slot, _)| field_slot_dto(slot));
            view.belief = belief_battle_state(belief).map(|fog| BeliefView {
                clauses: fog
                    .predicates
                    .iter()
                    .map(|clause| describe_clause(clause, fog))
                    .collect(),
            });
        }
        MatchState::GameOverState {
            winner,
            final_state,
            ..
        } => {
            view.winner = Some(player_dto(*winner));
            // Show the field as it stood when the battle ended (fainted mon,
            // final HP) behind the winner overlay.
            view.turn_number = final_state.turn_number;
            view.p1 = Some(side_view(
                final_state,
                Player::P1,
                belief,
                perspective,
                legal_items,
            ));
            view.p2 = Some(side_view(
                final_state,
                Player::P2,
                belief,
                perspective,
                legal_items,
            ));
            // See the matching comment in the `BattleState` arm above — same
            // belief-vs-ground-truth dispatch applies at game over.
            view.field = Some(match belief_battle_state(belief) {
                Some(fog) => field_view_from_belief(fog),
                None => field_view(final_state),
            });
            // Mirror the BattleState arm: the belief is still tracked (session.rs
            // never clears belief_p1/belief_p2 at game over), so the Predicates tab's
            // final deductions should stay visible instead of vanishing the instant
            // the match ends.
            view.belief = belief_battle_state(belief).map(|fog| BeliefView {
                clauses: fog
                    .predicates
                    .iter()
                    .map(|clause| describe_clause(clause, fog))
                    .collect(),
            });
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
fn command_label(
    state: &BattleState,
    player: Player,
    slot_idx: usize,
    command: &BattleCommand,
) -> Option<String> {
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
        EventKind::DamageDealt { target, new_hp, .. } => EventKindDto::DamageDealt {
            target: field_slot_dto(*target),
            new_hp: observed_hp_dto(new_hp),
        },
        EventKind::Healed { target, new_hp, .. } => EventKindDto::Healed {
            target: field_slot_dto(*target),
            new_hp: observed_hp_dto(new_hp),
        },
        EventKind::SetHp { target, new_hp, .. } => EventKindDto::SetHp {
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

#[cfg(test)]
mod tests {
    use super::*;
    use poke_rust::information::unknowns::InformationMode;
    use poke_rust::simulator;
    use poke_rust::state::battle::MatchState;
    use poke_rust::state::dex_data::{parse_move_dex, parse_pokemon_dex};

    // Distinct, easily-identifiable moves/items/abilities per side so a mixup
    // between the two teams is unmistakable in assertions.
    const TEAM_P1: &str = "Aerodactyl @ Aerodactylite\nAbility: Unnerve\nLevel: 50\nEVs: 12 HP / 12 Atk / 9 Def / 1 SpD / 32 Spe\nJolly Nature\n- Rock Slide\n- Dual Wingbeat\n- Tailwind\n- Protect\n";
    const TEAM_P2: &str = "Dragonite @ Choice Band\nAbility: Multiscale\nLevel: 50\nEVs: 4 HP / 252 Atk / 252 Spe\nAdamant Nature\n- Extreme Speed\n- Outrage\n- Earthquake\n- Fire Punch\n";

    /// Regression for the P2-perspective display bugs: under P2's belief, physical
    /// P1's team-preview entry must carry P1's OWN open-sheet data (species, item,
    /// ability, moves) — not P2's, and not blank/hidden-style. Exercises the exact
    /// scenario the live diagnostic caught: `preview_view` mixing up which physical
    /// side's fog list masks which physical side's ground truth.
    #[test]
    fn preview_view_p2_perspective_shows_p1s_own_open_sheet_data_not_p2s() {
        let pokemon_dex = parse_pokemon_dex("../pokemon_info/showdownDex.txt");
        let move_dex = parse_move_dex("../pokemon_info/showdownMoves.txt");

        let preview = simulator::team_preview_state_from_team_strings(
            TEAM_P1,
            TEAM_P2,
            &pokemon_dex,
            &move_dex,
            1,
            1,
            true,
        );

        let belief_p2 = poke_rust::information::unknowns::UnknownMatchState::team_preview_open_sheet_from_perspective(
            Player::P2,
            &preview.p2_mons,
            &preview.p1_mons,
            &pokemon_dex,
            1,
            1,
            50,
            InformationMode::OpenTeamSheet,
            true,
        );

        let view = preview_view(&preview, Some(&belief_p2), Player::P2, None);

        assert_eq!(view.p1_mons.len(), 1);
        let p1_mon = &view.p1_mons[0];
        assert_eq!(
            p1_mon.species, "Aerodactyl",
            "P1 tab must show P1's species"
        );
        assert_eq!(
            p1_mon.item.as_deref(),
            Some("Aerodactylite"),
            "openSheet mode reveals items immediately — P1's item must show, not P2's ('Choice Band') or blank"
        );
        assert_eq!(
            p1_mon.ability, "Unnerve",
            "P1's ability must show (Unnerve), not P2's (Multiscale)"
        );
        let move_names: Vec<&str> = p1_mon
            .moves
            .iter()
            .flatten()
            .map(|m| m.name.as_str())
            .collect();
        assert_eq!(
            move_names,
            vec!["Rock Slide", "Dual Wingbeat", "Tailwind", "Protect"],
            "openSheet mode reveals all 4 moves immediately — must be P1's own moves, not P2's or blank '???'s"
        );

        // Sanity: physical P2's own tab (the belief's own viewer) must be fully known
        // ground truth, unmasked.
        let p2_mon = &view.p2_mons[0];
        assert_eq!(p2_mon.species, "Dragonite");
        assert_eq!(p2_mon.item.as_deref(), Some("Choice Band"));
        assert_eq!(p2_mon.ability, "Multiscale");
    }

    /// Companion check at battle phase (not just team preview): P2's belief must
    /// correctly report physical P1's OTHER (non-active) mon under `possibleBack`,
    /// not P2's own roster — the second bug the live diagnostic caught.
    #[test]
    fn side_view_p2_perspective_possible_back_shows_p1s_roster_not_p2s() {
        let pokemon_dex = parse_pokemon_dex("../pokemon_info/showdownDex.txt");
        let move_dex = parse_move_dex("../pokemon_info/showdownMoves.txt");

        let team_p1_two = format!(
            "{TEAM_P1}\nCharizard @ Charizardite Y\nAbility: Blaze\nLevel: 50\nEVs: 32 HP / 10 Def / 11 SpA / 13 Spe\nModest Nature\n- Heat Wave\n- Weather Ball\n- Solar Beam\n- Protect\n"
        );
        let team_p2_two = format!(
            "{TEAM_P2}\nTyranitar @ Sitrus Berry\nAbility: Sand Stream\nLevel: 50\nEVs: 252 HP / 4 Atk / 252 SpD\nCareful Nature\n- Rock Slide\n- Crunch\n- Earthquake\n- Protect\n"
        );

        let preview = simulator::team_preview_state_from_team_strings(
            &team_p1_two,
            &team_p2_two,
            &pokemon_dex,
            &move_dex,
            1,
            2,
            true,
        );
        let p1_tp = poke_rust::state::battle::TeamPreviewCommand {
            active_indices: vec![0],
            back_indices: vec![1],
        };
        let p2_tp = p1_tp.clone();
        let p1_cmd = poke_rust::state::battle::PlayerCommand::TeamPreview(p1_tp.clone());
        let p2_cmd = poke_rust::state::battle::PlayerCommand::TeamPreview(p2_tp.clone());

        let UnknownMatchState::TeamPreview(tp_belief_p2) =
            poke_rust::information::unknowns::UnknownMatchState::team_preview_open_sheet_from_perspective(
                Player::P2, &preview.p2_mons, &preview.p1_mons, &pokemon_dex, 1, 2, 50,
                InformationMode::OpenTeamSheet, true,
            )
        else {
            panic!("expected TeamPreview");
        };
        let battle_belief_p2 = UnknownMatchState::Battle(tp_belief_p2.into_battle_state(
            Player::P2,
            &p1_tp.active_indices,
            &p1_tp.back_indices,
            &p2_tp.active_indices,
            &p2_tp.back_indices,
        ));

        // Resolve the team-preview -> battle transition through the same public
        // entry point session.rs uses, requesting events masked for P2 — mirrors
        // `advance_belief`'s real pipeline (seed via `into_battle_state`, then let
        // `apply_information` walk the transition's own switch-in event log to pull
        // each side's lead out of `possible_back` into `active`).
        let (next_state, events, _prob) = simulator::sample_turn(
            &MatchState::TeamPreviewState(preview),
            &p1_cmd,
            &p2_cmd,
            &move_dex,
            &pokemon_dex,
            false,
            1,
            Some(Player::P2),
        );
        let MatchState::BattleState(battle_state) = next_state else {
            panic!("expected BattleState")
        };
        let events = events.expect("observer set — events must be Some");
        let inference_config = poke_rust::information::inference::InferenceConfig {
            use_stat_points: true,
            force_max_ivs: true,
            ..Default::default()
        };
        let battle_belief_p2 = poke_rust::information::inference::apply_information(
            battle_belief_p2,
            &events,
            false,
            &pokemon_dex,
            &move_dex,
            &std::collections::HashMap::new(),
            &inference_config,
        );

        let view = side_view(
            &battle_state,
            Player::P1,
            Some(&battle_belief_p2),
            Player::P2,
            None,
        );
        let possible_back_species: Vec<&str> = view
            .possible_back
            .iter()
            .map(|m| m.species.as_str())
            .collect();
        assert_eq!(
            possible_back_species,
            vec!["Charizard"],
            "P1's possibleBack under P2's perspective must list P1's own bench (Charizard), not P2's roster"
        );
    }

    #[test]
    fn turn_range_computes_min_max_from_unknown_u8() {
        assert_eq!(turn_range(&Unknown::Known(5)), Some((5, 5)));
        assert_eq!(turn_range(&Unknown::Possibly(vec![5, 8])), Some((5, 8)));
        assert_eq!(turn_range(&Unknown::Possibly(vec![8, 5])), Some((5, 8)));
        assert_eq!(turn_range(&Unknown::Not(vec![5])), None);
    }

    #[test]
    fn named_turns_ranged_omits_max_only_when_collapsed() {
        let exact = named_turns_ranged("Sandstorm".to_string(), Some((5, 5)));
        assert_eq!(exact.turns, Some(5));
        assert_eq!(exact.turns_max, None);

        let ranged = named_turns_ranged("Sandstorm".to_string(), Some((5, 8)));
        assert_eq!(ranged.turns, Some(5));
        assert_eq!(ranged.turns_max, Some(8));

        let unknown = named_turns_ranged("Sandstorm".to_string(), None);
        assert_eq!(unknown.turns, None);
        assert_eq!(unknown.turns_max, None);
    }

    /// Regression: `battle_view`'s field output (weather/terrain/side-condition
    /// durations) must be masked through the belief exactly like per-mon data
    /// is. Before this fix, `field_view(battle)` (ground truth) was called
    /// UNCONDITIONALLY regardless of `perspective`/information mode, leaking
    /// the exact remaining weather-turn count to a player who should only be
    /// able to bound it — here, Sand Stream's base-5 duration vs. the 5-8
    /// range a Closed Team Sheet viewer is stuck with until the setter's
    /// item (an extension rock or not) is revealed.
    #[test]
    fn battle_view_masks_weather_turns_as_a_range_not_exact_ground_truth() {
        let pokemon_dex = parse_pokemon_dex("../pokemon_info/showdownDex.txt");
        let move_dex = parse_move_dex("../pokemon_info/showdownMoves.txt");

        // Sitrus Berry isn't an extension rock, so the real duration is
        // exactly the base 5 — but P1 (Closed Team Sheet) can't see that.
        let team_p2_sandstream = "Tyranitar @ Sitrus Berry\nAbility: Sand Stream\nLevel: 50\nEVs: 252 HP / 4 Atk / 252 SpD\nCareful Nature\n- Rock Slide\n- Crunch\n- Earthquake\n- Protect\n";

        let preview = simulator::team_preview_state_from_team_strings(
            TEAM_P1,
            team_p2_sandstream,
            &pokemon_dex,
            &move_dex,
            1,
            1,
            true,
        );
        let p1_tp = poke_rust::state::battle::TeamPreviewCommand {
            active_indices: vec![0],
            back_indices: vec![],
        };
        let p2_tp = p1_tp.clone();
        let p1_cmd = poke_rust::state::battle::PlayerCommand::TeamPreview(p1_tp.clone());
        let p2_cmd = poke_rust::state::battle::PlayerCommand::TeamPreview(p2_tp.clone());

        let UnknownMatchState::TeamPreview(tp_belief_p1) =
            poke_rust::information::unknowns::UnknownMatchState::team_preview_closed_sheet_from_perspective(
                Player::P1, &preview.p1_mons, &preview.p2_mons, &pokemon_dex, 1, 1, 50, true,
            )
        else {
            panic!("expected TeamPreview");
        };
        let battle_belief_p1 = UnknownMatchState::Battle(tp_belief_p1.into_battle_state(
            Player::P1,
            &p1_tp.active_indices,
            &p1_tp.back_indices,
            &p2_tp.active_indices,
            &p2_tp.back_indices,
        ));

        let (next_state, events, _prob) = simulator::sample_turn(
            &MatchState::TeamPreviewState(preview),
            &p1_cmd,
            &p2_cmd,
            &move_dex,
            &pokemon_dex,
            true,
            16,
            Some(Player::P1),
        );
        let ground_truth_weather_turns = match &next_state {
            MatchState::BattleState(b) => b.weather_turns,
            _ => panic!("expected BattleState"),
        };
        assert_eq!(ground_truth_weather_turns, Some(5));

        let events = events.expect("observer set — events must be Some");
        let inference_config = poke_rust::information::inference::InferenceConfig {
            use_stat_points: true,
            force_max_ivs: true,
            ..Default::default()
        };
        let battle_belief_p1 = poke_rust::information::inference::apply_information(
            battle_belief_p1,
            &events,
            false,
            &pokemon_dex,
            &move_dex,
            &std::collections::HashMap::new(),
            &inference_config,
        );

        let view = battle_view(&next_state, 1, 1, Some(&battle_belief_p1), Player::P1, None);
        let field = view.field.expect("battle phase always has a field view");
        let weather = field.weather.expect("Sand Stream should have set weather");
        assert_eq!(weather.turns, Some(5), "lower bound should still be the base duration");
        assert_eq!(
            weather.turns_max,
            Some(8),
            "P1 hasn't seen P2's item — the belief can't rule out an extension rock, so the upper \
             bound must show 8, not collapse to ground truth's exact 5 (the leak this test guards against)"
        );
    }

    /// Regression: a fainted-then-replaced opponent mon recorded in the belief's
    /// `p{1,2}_fainted_mons` bucket (see `UnknownBattleState` and the fix to
    /// `bench_outgoing_mon` in `inference.rs`) must be forwarded into
    /// `SideView.fainted`, not dropped by the mapping layer. This is the DTO-layer
    /// half of the fix; `inference_tests.rs` covers the engine populating the
    /// bucket in the first place.
    #[test]
    fn side_view_forwards_belief_fainted_bucket() {
        let pokemon_dex = parse_pokemon_dex("../pokemon_info/showdownDex.txt");
        let move_dex = parse_move_dex("../pokemon_info/showdownMoves.txt");

        let preview = simulator::team_preview_state_from_team_strings(
            TEAM_P1,
            TEAM_P2,
            &pokemon_dex,
            &move_dex,
            1,
            1,
            true,
        );
        let p1_tp = poke_rust::state::battle::TeamPreviewCommand {
            active_indices: vec![0],
            back_indices: vec![],
        };
        let p2_tp = p1_tp.clone();
        let p1_cmd = poke_rust::state::battle::PlayerCommand::TeamPreview(p1_tp.clone());
        let p2_cmd = poke_rust::state::battle::PlayerCommand::TeamPreview(p2_tp.clone());

        // Real ground-truth BattleState, exactly as the server produces it.
        let (next_state, _events, _prob) = simulator::sample_turn(
            &MatchState::TeamPreviewState(preview.clone()),
            &p1_cmd,
            &p2_cmd,
            &move_dex,
            &pokemon_dex,
            false,
            1,
            None,
        );
        let MatchState::BattleState(battle_state) = next_state else {
            panic!("expected BattleState")
        };

        // P1's belief about P2 — battle-phase, then hand-seed a fainted opponent
        // mon into the (otherwise untouched) fainted bucket. How the bucket gets
        // populated during a real battle is covered by the inference-engine
        // regression test; this test only exercises the belief -> DTO forwarding.
        let UnknownMatchState::TeamPreview(tp_belief_p1) =
            poke_rust::information::unknowns::UnknownMatchState::team_preview_open_sheet_from_perspective(
                Player::P1, &preview.p1_mons, &preview.p2_mons, &pokemon_dex, 1, 1, 50,
                InformationMode::OpenTeamSheet, true,
            )
        else {
            panic!("expected TeamPreview");
        };
        let mut battle_belief_p1 = tp_belief_p1.into_battle_state(
            Player::P1,
            &p1_tp.active_indices,
            &p1_tp.back_indices,
            &p2_tp.active_indices,
            &p2_tp.back_indices,
        );
        let mut fainted_mon = UnknownPokemonState::from_opponent_species(
            poke_rust::data::species::Species::Snorlax,
            &pokemon_dex,
            50,
        );
        fainted_mon.fainted = true;
        battle_belief_p1.p2_fainted_mons.push(fainted_mon);
        let belief_p1 = UnknownMatchState::Battle(battle_belief_p1);

        // Mask P2 (the opponent) from P1's perspective — the exact call the server
        // makes to render the opponent's sidebar under fog-of-war.
        let view = side_view(
            &battle_state,
            Player::P2,
            Some(&belief_p1),
            Player::P1,
            None,
        );

        assert_eq!(
            view.fainted.len(),
            1,
            "belief's p2_fainted_mons must surface as SideView.fainted"
        );
        assert_eq!(view.fainted[0].species, "Snorlax");
        assert!(
            view.fainted[0].fainted,
            "forwarded fainted-bucket entry must keep fainted: true"
        );
    }

    /// TODO.md: "Should not display Not of items that are not even in the current
    /// format!" — end-to-end through the real DTO render path (`side_view` ->
    /// `mask_pokemon_view` -> `describe_unknown_item_union`), not just the
    /// `describe_unknown_item` unit tests. An opponent mon whose item lattice has
    /// excluded one in-whitelist item (Choice Band) and one out-of-whitelist item
    /// (Rocky Helmet — not in this format's pool at all) must render only the
    /// in-format exclusion.
    #[test]
    fn side_view_item_text_hides_out_of_format_exclusions() {
        let pokemon_dex = parse_pokemon_dex("../pokemon_info/showdownDex.txt");
        let move_dex = parse_move_dex("../pokemon_info/showdownMoves.txt");

        let preview = simulator::team_preview_state_from_team_strings(
            TEAM_P1,
            TEAM_P2,
            &pokemon_dex,
            &move_dex,
            1,
            1,
            true,
        );
        let p1_tp = poke_rust::state::battle::TeamPreviewCommand {
            active_indices: vec![0],
            back_indices: vec![],
        };
        let p2_tp = p1_tp.clone();
        let p1_cmd = poke_rust::state::battle::PlayerCommand::TeamPreview(p1_tp.clone());
        let p2_cmd = poke_rust::state::battle::PlayerCommand::TeamPreview(p2_tp.clone());

        let (next_state, _events, _prob) = simulator::sample_turn(
            &MatchState::TeamPreviewState(preview.clone()),
            &p1_cmd,
            &p2_cmd,
            &move_dex,
            &pokemon_dex,
            false,
            1,
            None,
        );
        let MatchState::BattleState(battle_state) = next_state else {
            panic!("expected BattleState")
        };

        let UnknownMatchState::TeamPreview(tp_belief_p1) =
            poke_rust::information::unknowns::UnknownMatchState::team_preview_closed_sheet_from_perspective(
                Player::P1, &preview.p1_mons, &preview.p2_mons, &pokemon_dex, 1, 1, 50, true,
            )
        else {
            panic!("expected TeamPreview");
        };
        let mut battle_belief_p1 = tp_belief_p1.into_battle_state(
            Player::P1,
            &p1_tp.active_indices,
            &p1_tp.back_indices,
            &p2_tp.active_indices,
            &p2_tp.back_indices,
        );
        // Hand-seed the exclusion this test cares about, isolated from whatever
        // real inference would derive — mirrors `side_view_forwards_belief_fainted_bucket`'s
        // hand-seeding approach for the same reason (deterministic, no dependency
        // on a specific event sequence actually producing this exact lattice).
        // `into_battle_state` parks the whole non-viewer roster in `possible_back`
        // until the transition's own events are replayed (see its doc comment) — P2's
        // lead hasn't been pulled into `p2_active_mons` yet, so seed the exclusion on
        // the roster entry that's actually populated right now.
        battle_belief_p1.p2_possible_back_mons[0].item =
            Unknown::Not(vec![Item::ChoiceBand, Item::RockyHelmet]);
        let belief_p1 = UnknownMatchState::Battle(battle_belief_p1);

        let legal: HashSet<Item> = [Item::ChoiceBand, Item::ChoiceScarf, Item::Leftovers]
            .into_iter()
            .collect();
        let view = side_view(
            &battle_state,
            Player::P2,
            Some(&belief_p1),
            Player::P1,
            Some(&legal),
        );

        assert_eq!(
            view.possible_back[0].item.as_deref(),
            Some("not Choice Band"),
            "Rocky Helmet was never in this format's pool, so it must not appear in the rendered exclusion list"
        );
    }

    /// Shared setup for the two `choice_lock_masking_*` tests below: a real
    /// ground-truth battle where P2's active mon has been hand-seeded with a
    /// `ChoiceLock` volatile (mirroring exactly how `simulator/mod.rs` sets it —
    /// `TurnStatus(ChoiceLock(move), 0)` — the instant a Choice-item holder's move
    /// resolves), and a fresh battle-phase belief for P1 about P2 with P2's only
    /// mon already sitting in `p2_active_mons` at the matching index (skipping the
    /// team-preview -> battle event replay `into_battle_state` normally expects —
    /// this test only needs a belief mon AT the active slot, not a fully-populated
    /// send-in sequence).
    fn choice_lock_test_fixture() -> (
        BattleState,
        poke_rust::information::unknowns::UnknownBattleState,
    ) {
        let pokemon_dex = parse_pokemon_dex("../pokemon_info/showdownDex.txt");
        let move_dex = parse_move_dex("../pokemon_info/showdownMoves.txt");

        let preview = simulator::team_preview_state_from_team_strings(
            TEAM_P1,
            TEAM_P2,
            &pokemon_dex,
            &move_dex,
            1,
            1,
            true,
        );
        let p1_tp = poke_rust::state::battle::TeamPreviewCommand {
            active_indices: vec![0],
            back_indices: vec![],
        };
        let p2_tp = p1_tp.clone();
        let p1_cmd = poke_rust::state::battle::PlayerCommand::TeamPreview(p1_tp.clone());
        let p2_cmd = poke_rust::state::battle::PlayerCommand::TeamPreview(p2_tp.clone());

        let (next_state, _events, _prob) = simulator::sample_turn(
            &MatchState::TeamPreviewState(preview.clone()),
            &p1_cmd,
            &p2_cmd,
            &move_dex,
            &pokemon_dex,
            false,
            1,
            None,
        );
        let MatchState::BattleState(mut battle_state) = next_state else {
            panic!("expected BattleState")
        };
        battle_state.p2_active_mons[0]
            .volatiles
            .push(VolatileStatusState::TurnStatus(
                VolatileStatus::ChoiceLock(
                    poke_rust::data::pokemon_move::PokemonMove::ExtremeSpeed,
                ),
                0,
            ));

        let UnknownMatchState::TeamPreview(tp_belief_p1) =
            poke_rust::information::unknowns::UnknownMatchState::team_preview_closed_sheet_from_perspective(
                Player::P1, &preview.p1_mons, &preview.p2_mons, &pokemon_dex, 1, 1, 50, true,
            )
        else {
            panic!("expected TeamPreview");
        };
        let mut battle_belief_p1 = tp_belief_p1.into_battle_state(
            Player::P1,
            &p1_tp.active_indices,
            &p1_tp.back_indices,
            &p2_tp.active_indices,
            &p2_tp.back_indices,
        );
        let p2_mon_belief = battle_belief_p1.p2_possible_back_mons.remove(0);
        battle_belief_p1.p2_active_mons.push(p2_mon_belief);

        (battle_state, battle_belief_p1)
    }

    /// TODO.md: "You can see Choice Lock volatile on your opponents mons even when
    /// you don't have enough information to be able to know they are choice
    /// locked." — with the belief still fully unsure about P2's item, the ground
    /// truth's `ChoiceLock` volatile must NOT leak into the masked opponent view.
    #[test]
    fn choice_lock_masking_hidden_when_item_unconfirmed() {
        let (battle_state, mut battle_belief_p1) = choice_lock_test_fixture();
        battle_belief_p1.p2_active_mons[0].item = Unknown::Not(vec![]);
        let belief_p1 = UnknownMatchState::Battle(battle_belief_p1);

        let view = side_view(
            &battle_state,
            Player::P2,
            Some(&belief_p1),
            Player::P1,
            None,
        );

        let names: Vec<&str> = view.active[0]
            .volatiles
            .iter()
            .map(|v| v.name.as_str())
            .collect();
        assert!(
            !names.iter().any(|n| n.starts_with("Choice Lock")),
            "Choice Lock must not be visible while the belief hasn't confirmed a Choice item; got {names:?}"
        );
    }

    /// Companion case: once the belief HAS confirmed the held item is a Choice
    /// item (its state can now be guaranteed, per the TODO's own wording), Choice
    /// Lock must render normally like any other observable volatile.
    #[test]
    fn choice_lock_masking_shown_when_item_confirmed_choice() {
        let (battle_state, mut battle_belief_p1) = choice_lock_test_fixture();
        battle_belief_p1.p2_active_mons[0].item = Unknown::Known(Item::ChoiceBand);
        let belief_p1 = UnknownMatchState::Battle(battle_belief_p1);

        let view = side_view(
            &battle_state,
            Player::P2,
            Some(&belief_p1),
            Player::P1,
            None,
        );

        let names: Vec<&str> = view.active[0]
            .volatiles
            .iter()
            .map(|v| v.name.as_str())
            .collect();
        assert!(
            names.iter().any(|n| n.starts_with("Choice Lock")),
            "Choice Lock must render once the belief has confirmed a Choice item; got {names:?}"
        );
    }

    /// Closed Team Sheet mode (the traditional VGC/Champions competitive format):
    /// at team preview only the opponent's species should be visible — item,
    /// ability, and all 4 moves must render as the standard "unknown" placeholders,
    /// unlike Open Team Sheet mode's immediate reveal (see the companion test above).
    #[test]
    fn preview_view_closed_sheet_masks_item_ability_moves_but_shows_species() {
        let pokemon_dex = parse_pokemon_dex("../pokemon_info/showdownDex.txt");
        let move_dex = parse_move_dex("../pokemon_info/showdownMoves.txt");

        let preview = simulator::team_preview_state_from_team_strings(
            TEAM_P1,
            TEAM_P2,
            &pokemon_dex,
            &move_dex,
            1,
            1,
            true,
        );

        let belief_p2 = poke_rust::information::unknowns::UnknownMatchState::team_preview_closed_sheet_from_perspective(
            Player::P2,
            &preview.p2_mons,
            &preview.p1_mons,
            &pokemon_dex,
            1,
            1,
            50,
            true,
        );

        let view = preview_view(&preview, Some(&belief_p2), Player::P2, None);

        let p1_mon = &view.p1_mons[0];
        assert_eq!(
            p1_mon.species, "Aerodactyl",
            "species must still be visible at team preview"
        );
        assert_eq!(
            p1_mon.item.as_deref(),
            Some("Unknown"),
            "closedSheet mode must not reveal item at preview"
        );
        // `from_opponent_species` narrows ability to the species' small dex candidate
        // set (Aerodactyl: Rock Head/Pressure/Unnerve) rather than a blanket "Unknown"
        // — that's real bounded uncertainty, not full opacity. The assertion that
        // matters for closedSheet is that the TRUE single ability isn't singled out,
        // unlike openSheet's exact "Unnerve" reveal (see the companion test above).
        assert_ne!(
            p1_mon.ability, "Unnerve",
            "closedSheet mode must not single out the true ability the way openSheet does"
        );
        assert!(
            p1_mon.ability.contains("Unnerve"),
            "the true ability must still be among the candidate set: {}",
            p1_mon.ability
        );
        let move_names: Vec<&str> = p1_mon
            .moves
            .iter()
            .flatten()
            .map(|m| m.name.as_str())
            .collect();
        assert_eq!(
            move_names,
            vec!["???", "???", "???", "???"],
            "closedSheet mode must not reveal any moves at preview"
        );

        // Sanity: physical P2's own tab (the belief's own viewer) stays fully known
        // ground truth, unmasked, exactly as under any other information mode.
        let p2_mon = &view.p2_mons[0];
        assert_eq!(p2_mon.species, "Dragonite");
        assert_eq!(p2_mon.item.as_deref(), Some("Choice Band"));
        assert_eq!(p2_mon.ability, "Multiscale");
    }

    /// S34-style regression for the closed-sheet path specifically: with
    /// `force_max_ivs` set, the opponent's min-stat bound must be tightened to IV 31
    /// (via `pin_min_ivs_to_max`), not left at the untightened IV-0 floor
    /// `from_opponent_species` assumes by default. Compares the two directly rather
    /// than hardcoding an expected stat value, so the assertion doesn't depend on
    /// `calc_stat`'s formula.
    #[test]
    fn closed_sheet_force_max_ivs_tightens_min_stat_bound() {
        let pokemon_dex = parse_pokemon_dex("../pokemon_info/showdownDex.txt");
        let move_dex = parse_move_dex("../pokemon_info/showdownMoves.txt");

        let preview = simulator::team_preview_state_from_team_strings(
            TEAM_P1,
            TEAM_P2,
            &pokemon_dex,
            &move_dex,
            1,
            1,
            true,
        );

        let belief_pinned = poke_rust::information::unknowns::UnknownMatchState::team_preview_closed_sheet_from_perspective(
            Player::P2, &preview.p2_mons, &preview.p1_mons, &pokemon_dex, 1, 1, 50, true,
        );
        let belief_unpinned = poke_rust::information::unknowns::UnknownMatchState::team_preview_closed_sheet_from_perspective(
            Player::P2, &preview.p2_mons, &preview.p1_mons, &pokemon_dex, 1, 1, 50, false,
        );

        let view_pinned = preview_view(&preview, Some(&belief_pinned), Player::P2, None);
        let view_unpinned = preview_view(&preview, Some(&belief_unpinned), Player::P2, None);

        let stats_pinned = view_pinned.p1_mons[0].stats;
        let stats_unpinned = view_unpinned.p1_mons[0].stats;
        assert!(
            stats_pinned
                .iter()
                .zip(stats_unpinned.iter())
                .all(|(p, u)| p >= u),
            "force_max_ivs must never lower a min-stat bound: pinned {stats_pinned:?} vs unpinned {stats_unpinned:?}"
        );
        assert!(
            stats_pinned
                .iter()
                .zip(stats_unpinned.iter())
                .any(|(p, u)| p > u),
            "force_max_ivs must strictly tighten at least one min-stat bound (IV 31 floor vs IV 0 floor): pinned {stats_pinned:?} vs unpinned {stats_unpinned:?}"
        );
    }
}
