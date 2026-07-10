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
use crate::information::inference::get_mon_by_idx;
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
                    if excluded.len() <= possible.len() {
                        excluded
                            .iter()
                            .map(|i| format!("not {}", display_name(i)))
                            .collect::<Vec<_>>()
                            .join(", ")
                    } else {
                        possible.iter().map(|i| display_name(i)).collect::<Vec<_>>().join(" or ")
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
