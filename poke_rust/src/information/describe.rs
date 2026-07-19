//! Human-readable rendering of the fog-of-war belief state.
//!
//! `Unknown<T>`, `UnknownPokemonState`, and `Statement` have no `Display`/`Serialize`
//! anywhere else in the engine — this is the one place that turns them into plain
//! English, consumed by the server's masked `PokemonView` mapping and the
//! "Predicates" tab DTO. Reuses `user::humanize_identifier`/`user::move_name`, the
//! same enum-Debug-to-display-name convention the server's `mapping.rs` already uses
//! for ground-truth DTOs, so masked and unmasked text reads consistently.

use std::collections::HashSet;

use crate::data::item::Item;
use crate::data::pokemon_move::PokemonMove;
use crate::information::inference::{get_mon_by_idx, unknown_union};
use crate::information::unknowns::{Statement, Unknown, UnknownBattleState};
use crate::user::{humanize_identifier, move_name};

fn display_name<T: std::fmt::Debug>(value: &T) -> String {
    humanize_identifier(format!("{:?}", value))
}

/// Render an `Unknown<T>` field as plain English: `Known` is just the name;
/// `Possibly` joins the candidates with " or "; `Not` collapses to "Unknown" — for
/// most fields a potentially-long exclusion list isn't worth rendering (see
/// `describe_unknown_item` below for the one field where it is).
pub fn describe_unknown<T: std::fmt::Debug>(u: &Unknown<T>) -> String {
    match u {
        Unknown::Known(x) => display_name(x),
        Unknown::Possibly(candidates) if !candidates.is_empty() => {
            candidates.iter().map(display_name).collect::<Vec<_>>().join(" or ")
        }
        Unknown::Possibly(_) | Unknown::Not(_) => "Unknown".to_string(),
    }
}

/// Render an opponent's item knowledge, choosing whichever phrasing is shorter: the
/// possible-item list, or the impossible-item list. `legal_items` mirrors
/// `InferenceConfig.legal_items` — `Some(pool)` is a bounded whitelist (a format's
/// legal items), against which "all minus excluded" is cheap and often the shorter
/// description; `None` means every one of the ~1,000 items is technically possible,
/// so the excluded list is almost always shorter and is rendered directly.
///
/// Returns the *full* list either way — the frontend truncates to 3 with a
/// click-to-expand affordance, so this function doesn't need to know about that.
pub fn describe_unknown_item(u: &Unknown<Item>, legal_items: Option<&HashSet<Item>>) -> String {
    match u {
        Unknown::Known(item) => display_name(item),
        Unknown::Possibly(candidates) => {
            if candidates.is_empty() {
                "Unknown".to_string()
            } else {
                candidates.iter().map(display_name).collect::<Vec<_>>().join(" or ")
            }
        }
        Unknown::Not(excluded) => {
            if excluded.is_empty() {
                return "Unknown".to_string();
            }
            match legal_items {
                Some(pool) => {
                    let possible: Vec<&Item> =
                        pool.iter().filter(|i| !excluded.contains(i)).collect();
                    // Only exclusions that were ever actually possible under this
                    // format's pool are worth naming — an item banned outright was
                    // never a candidate to begin with, so listing it as "not X"
                    // alongside genuine deductions is just noise (and, worse, is
                    // misleading: it reads as "we ruled this out," not "this was
                    // never legal here"). Restrict the "not X" rendering to
                    // `excluded ∩ pool` before comparing list lengths.
                    let in_format_excluded: Vec<&Item> =
                        excluded.iter().filter(|i| pool.contains(i)).collect();
                    if in_format_excluded.is_empty() {
                        return "Unknown".to_string();
                    }
                    if in_format_excluded.len() <= possible.len() {
                        in_format_excluded
                            .iter()
                            .map(|i| format!("not {}", display_name(i)))
                            .collect::<Vec<_>>()
                            .join(", ")
                    } else {
                        possible.iter().map(display_name).collect::<Vec<_>>().join(" or ")
                    }
                }
                // No whitelist: with ~1,000 items in the pool, a short exclusion
                // list is virtually always the shorter description.
                None => excluded
                    .iter()
                    .map(|i| format!("not {}", display_name(i)))
                    .collect::<Vec<_>>()
                    .join(", "),
            }
        }
    }
}

/// Resolve a `Statement`'s `mon_idx` to a display name via `get_mon_by_idx`. Falls
/// back to a placeholder if the index is somehow out of range — this is a
/// display-only concern that must never panic, unlike the engine's soundness checks.
fn mon_label(battle: &UnknownBattleState, mon_idx: usize) -> String {
    get_mon_by_idx(battle, mon_idx)
        .map(|m| describe_unknown(&m.possible_species))
        .unwrap_or_else(|| "an unknown Pokémon".to_string())
}

/// Phrase one `Statement` literal in plain English, one arm per variant.
pub fn describe_statement(stmt: &Statement, battle: &UnknownBattleState) -> String {
    match stmt {
        Statement::Not(inner) => format!("NOT ({})", describe_statement(inner, battle)),
        Statement::HasItem { mon_idx, item } => {
            format!("{}'s item is {}", mon_label(battle, *mon_idx), display_name(item))
        }
        Statement::HasAbility { mon_idx, ability } => {
            format!("{}'s ability is {}", mon_label(battle, *mon_idx), display_name(ability))
        }
        Statement::WeatherTurns { turns } => {
            format!("The weather lasts {} more turn(s)", turns)
        }
        Statement::TerrainTurns { turns } => {
            format!("The terrain lasts {} more turn(s)", turns)
        }
        Statement::SideConditionTurns { side, side_condition, turns } => {
            format!(
                "{:?}'s {} lasts {} more turn(s)",
                side,
                display_name(side_condition),
                turns
            )
        }
        Statement::NatureBoostsStat { mon_idx, stat } => {
            format!("{}'s nature boosts {}", mon_label(battle, *mon_idx), display_name(stat))
        }
        Statement::NatureNerfsStat { mon_idx, stat } => {
            format!("{}'s nature lowers {}", mon_label(battle, *mon_idx), display_name(stat))
        }
        Statement::EVIVStatGE { mon_idx, stat, value } => {
            format!(
                "{}'s {} (pre-nature) is at least {}",
                mon_label(battle, *mon_idx),
                display_name(stat),
                value
            )
        }
        Statement::EVIVStatLE { mon_idx, stat, value } => {
            format!(
                "{}'s {} (pre-nature) is at most {}",
                mon_label(battle, *mon_idx),
                display_name(stat),
                value
            )
        }
        Statement::SpeedComparison { fast_idx, slow_idx, .. } => {
            format!(
                "{} is faster than {}",
                mon_label(battle, *fast_idx),
                mon_label(battle, *slow_idx)
            )
        }
        Statement::KnowsThreateningMove { mon_idx, defender_types } => {
            let types = defender_types.iter().map(display_name).collect::<Vec<_>>().join("/");
            format!(
                "{} knows a move super-effective against {}",
                mon_label(battle, *mon_idx),
                types
            )
        }
    }
}

/// Join a CNF clause's literals with " OR " — this IS the "list of ORs" the
/// Predicates tab renders, one string per entry in `UnknownBattleState.predicates`.
pub fn describe_clause(clause: &[Statement], battle: &UnknownBattleState) -> String {
    clause.iter().map(|s| describe_statement(s, battle)).collect::<Vec<_>>().join(" OR ")
}

/// A move slot rendered for the masked opponent view: the revealed move's name, or
/// the literal placeholder for an unrevealed slot (never omitted — the UI always
/// shows all 4 slots).
pub fn describe_move_slot(slot: Option<crate::data::pokemon_move::PokemonMove>) -> String {
    match slot {
        Some(m) => move_name(&m),
        None => "???".to_string(),
    }
}

/// Render an `Unknown<T>` field as the union of a primary value and a live Zoroark
/// hypothesis's own value for the same field, when one exists — "A or B" when they
/// differ, just "A" when they agree or the hypothesis has nothing more specific.
/// `hypothesis` is `None` when the mon carries no live `possible_illusion_state`
/// (the overwhelming majority of mons), in which case this is identical to
/// `describe_unknown(primary)`.
pub fn describe_unknown_union<T: std::fmt::Debug + PartialEq + Clone>(
    primary: &Unknown<T>,
    hypothesis: Option<&Unknown<T>>,
) -> String {
    match hypothesis {
        Some(h) => describe_unknown(&unknown_union(primary, h)),
        None => describe_unknown(primary),
    }
}

/// Item variant of `describe_unknown_union` — see that function's doc comment.
/// Unioning first, then describing, keeps the "shorter of possible/impossible
/// phrasing" logic in `describe_unknown_item` as the single source of truth.
pub fn describe_unknown_item_union(
    primary: &Unknown<Item>,
    hypothesis: Option<&Unknown<Item>>,
    legal_items: Option<&HashSet<Item>>,
) -> String {
    match hypothesis {
        Some(h) => describe_unknown_item(&unknown_union(primary, h), legal_items),
        None => describe_unknown_item(primary, legal_items),
    }
}

/// Render one move slot as a union across two hypotheses (primary + a live Zoroark
/// sub-state, if any): "A or B" when both are revealed and differ, just "A" when
/// only one is revealed or both agree, "???" when neither is revealed. Slot-index
/// pairing between two different species' movesets is a display convenience, not a
/// semantic correspondence — this is what lets a suspected Zoroark disguise show
/// e.g. "Body Slam or Nasty Plot" per slot instead of always "???".
pub fn describe_move_slot_union(primary: Option<PokemonMove>, hypothesis: Option<PokemonMove>) -> String {
    match (primary, hypothesis) {
        (Some(p), Some(h)) if p != h => format!("{} or {}", move_name(&p), move_name(&h)),
        (Some(p), _) => move_name(&p),
        (None, Some(h)) => move_name(&h),
        (None, None) => "???".to_string(),
    }
}
