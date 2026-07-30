//! Resolves usage-site display names to engine enums.
//!
//! Generated parsers return an Unknown variant after a failed match.
//! These resolvers return `None` instead.
//! An unknown species would otherwise use incorrect fallback base stats.
//!
//! Species names need overrides for regional and special forms.

use crate::data::ability::Ability;
use crate::data::item::Item;
use crate::data::pokemon_move::PokemonMove;
use crate::data::species::Species;
use crate::state::pokemon::{Nature, normalize_string};

/// Maps Champions species names that do not match generated enum names.
/// Keys use normalized display names and file slugs.
/// The explicit table keeps irregular form names auditable.
const SPECIES_OVERRIDES: &[(&str, Species)] = &[
    ("aegislashshieldforme", Species::Aegislash),
    ("alolanninetales", Species::NinetalesAlola),
    ("alolanraichu", Species::RaichuAlola),
    ("basculegionfemale", Species::BasculegionF),
    ("basculegionmale", Species::Basculegion),
    // The source uses `Fan Rotom` but suffix-first names for other Rotom forms.
    ("fanrotom", Species::RotomFan),
    ("florgesredflower", Species::Florges),
    ("furfrounaturalform", Species::Furfrou),
    ("galarianslowbro", Species::SlowbroGalar),
    ("galarianslowking", Species::SlowkingGalar),
    ("galarianstunfisk", Species::StunfiskGalar),
    ("gourgeistjumbovariety", Species::GourgeistSuper),
    ("gourgeistlargevariety", Species::GourgeistLarge),
    ("gourgeistsmallvariety", Species::GourgeistSmall),
    ("hisuianarcanine", Species::ArcanineHisui),
    ("hisuianavalugg", Species::AvaluggHisui),
    ("hisuiandecidueye", Species::DecidueyeHisui),
    ("hisuiangoodra", Species::GoodraHisui),
    ("hisuiansamurott", Species::SamurottHisui),
    ("hisuiantyphlosion", Species::TyphlosionHisui),
    ("hisuianzoroark", Species::ZoroarkHisui),
    ("lycanrocduskform", Species::LycanrocDusk),
    ("lycanrocmidnightform", Species::LycanrocMidnight),
    ("mausholdfamilyoffour", Species::MausholdFour),
    ("meowsticfemale", Species::MeowsticF),
    ("palafinzeroform", Species::Palafin),
    ("paldeantaurosaquabreed", Species::TaurosPaldeaAqua),
    ("paldeantaurosblazebreed", Species::TaurosPaldeaBlaze),
    ("paldeantauroscombatbreed", Species::TaurosPaldeaCombat),
    ("vivillonfancypattern", Species::VivillonFancy),
];

/// Resolves a Champions species name or file slug.
/// Never returns `Species::Unknown`.
pub fn resolve_species(raw: &str) -> Option<Species> {
    let key = normalize_string(raw);
    if key.is_empty() {
        return None;
    }
    if let Some((_, species)) = SPECIES_OVERRIDES.iter().find(|(k, _)| *k == key) {
        return Some(species.clone());
    }
    match Species::from_str(&key) {
        Species::Unknown(_) | Species::None => None,
        species => Some(species),
    }
}

pub fn resolve_move(raw: &str) -> Option<PokemonMove> {
    if normalize_string(raw).is_empty() {
        return None;
    }
    match PokemonMove::from_str(raw) {
        PokemonMove::Unknown(_) => None,
        m => Some(m),
    }
}

pub fn resolve_item(raw: &str) -> Option<Item> {
    if normalize_string(raw).is_empty() {
        return None;
    }
    match Item::from_str(raw) {
        Item::Unknown(_) => None,
        i => Some(i),
    }
}

pub fn resolve_ability(raw: &str) -> Option<Ability> {
    if normalize_string(raw).is_empty() {
        return None;
    }
    match Ability::from_str(raw) {
        Ability::Unknown(_) => None,
        a => Some(a),
    }
}

/// Resolves a nature without case differences.
pub fn resolve_nature(raw: &str) -> Option<Nature> {
    Some(match normalize_string(raw).as_str() {
        "hardy" => Nature::Hardy,
        "lonely" => Nature::Lonely,
        "adamant" => Nature::Adamant,
        "naughty" => Nature::Naughty,
        "brave" => Nature::Brave,
        "bold" => Nature::Bold,
        "docile" => Nature::Docile,
        "impish" => Nature::Impish,
        "lax" => Nature::Lax,
        "relaxed" => Nature::Relaxed,
        "modest" => Nature::Modest,
        "mild" => Nature::Mild,
        "bashful" => Nature::Bashful,
        "rash" => Nature::Rash,
        "quiet" => Nature::Quiet,
        "calm" => Nature::Calm,
        "gentle" => Nature::Gentle,
        "careful" => Nature::Careful,
        "quirky" => Nature::Quirky,
        "sassy" => Nature::Sassy,
        "timid" => Nature::Timid,
        "hasty" => Nature::Hasty,
        "jolly" => Nature::Jolly,
        "naive" => Nature::Naive,
        "serious" => Nature::Serious,
        _ => return None,
    })
}

/// Resolves a nature from its raised and lowered stats.
/// This recovers rows with a damaged nature name.
/// Returns `None` for neutral or invalid pairs.
pub fn nature_from_stat_change(up: &str, down: &str) -> Option<Nature> {
    let up = stat_index(up)?;
    let down = stat_index(down)?;
    if up == down {
        return None;
    }
    ALL_NATURES.iter().copied().find(|nature| {
        let modifiers = crate::state::pokemon::nature_stat_modifiers(nature);
        modifiers[up] > 1.0 && modifiers[down] < 1.0
    })
}

/// Index into `nature_stat_modifiers`' `[atk, def, spa, spd, spe]`.
fn stat_index(name: &str) -> Option<usize> {
    Some(match normalize_string(name).as_str() {
        "attack" | "atk" => 0,
        "defense" | "defence" | "def" => 1,
        "spatk" | "specialattack" | "spa" => 2,
        "spdef" | "specialdefense" | "specialdefence" | "spd" => 3,
        "speed" | "spe" => 4,
        _ => return None,
    })
}

/// Every nature, for the uniform fallback when the meta offers none.
pub const ALL_NATURES: [Nature; 25] = [
    Nature::Hardy,
    Nature::Lonely,
    Nature::Adamant,
    Nature::Naughty,
    Nature::Brave,
    Nature::Bold,
    Nature::Docile,
    Nature::Impish,
    Nature::Lax,
    Nature::Relaxed,
    Nature::Modest,
    Nature::Mild,
    Nature::Bashful,
    Nature::Rash,
    Nature::Quiet,
    Nature::Calm,
    Nature::Gentle,
    Nature::Careful,
    Nature::Quirky,
    Nature::Sassy,
    Nature::Timid,
    Nature::Hasty,
    Nature::Jolly,
    Nature::Naive,
    Nature::Serious,
];

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn overrides_are_normalized_and_unique() {
        let mut seen = std::collections::HashSet::new();
        for (key, _) in SPECIES_OVERRIDES {
            assert_eq!(
                *key,
                normalize_string(key),
                "override key {key:?} is not in normalized form"
            );
            assert!(seen.insert(*key), "duplicate override key {key:?}");
        }
    }

    /// No two Champions names may collapse onto the same `Species`. A collision
    /// would mean two distinct formes with different base stats are being
    /// treated as one, which the base-form entries (`Aegislash Shield Forme` ->
    /// `Aegislash`, etc.) make a live possibility if the site ever adds a plain
    /// `aegislash` entry alongside the suffixed one.
    #[test]
    fn meta_species_map_is_injective() {
        let mut by_target: HashMap<Species, Vec<&str>> = HashMap::new();
        for (key, species) in SPECIES_OVERRIDES {
            by_target.entry(species.clone()).or_default().push(key);
        }
        let collisions: Vec<_> = by_target.iter().filter(|(_, ks)| ks.len() > 1).collect();
        assert!(collisions.is_empty(), "colliding overrides: {collisions:?}");
    }

    #[test]
    fn resolves_the_irregular_species() {
        // The ones no rule would get right.
        assert_eq!(
            resolve_species("Gourgeist Jumbo Variety"),
            Some(Species::GourgeistSuper)
        );
        assert_eq!(
            resolve_species("Maushold Family of Four"),
            Some(Species::MausholdFour)
        );
        assert_eq!(
            resolve_species("paldean-tauros-aqua-breed"),
            Some(Species::TaurosPaldeaAqua)
        );
        // Display form and file slug must agree.
        assert_eq!(
            resolve_species("Hisuian Zoroark"),
            resolve_species("hisuian-zoroark")
        );
        // Ordinary names still go through `from_str`.
        assert_eq!(resolve_species("Garchomp"), Some(Species::Garchomp));
    }

    /// The whole point of the `Option` return: an unrecognized name must not
    /// silently become `Species::Unknown` and inherit `[100; 6]` base stats.
    #[test]
    fn unresolvable_names_are_none_not_unknown() {
        assert_eq!(resolve_species("Notapokemon"), None);
        assert_eq!(resolve_species(""), None);
        assert_eq!(resolve_move(""), None);
        assert_eq!(resolve_item("Definitely Not An Item"), None);
        assert_eq!(resolve_ability(""), None);
        assert_eq!(resolve_nature("Jolly Nature"), None);
    }

    #[test]
    fn resolves_names_case_and_punctuation_insensitively() {
        assert_eq!(resolve_move("Dragon Claw"), resolve_move("dragonclaw"));
        assert_eq!(resolve_item("Life Orb"), resolve_item("life-orb"));
        assert_eq!(resolve_ability("Rough Skin"), resolve_ability("roughskin"));
        assert_eq!(resolve_nature("JOLLY"), Some(Nature::Jolly));
    }

    /// The stat pair determines the nature outright, which is what lets a row
    /// with a corrupted name still be recovered.
    #[test]
    fn recovers_a_nature_from_its_stat_pair() {
        // The real case: Singles Stunfisk rank 10, named "SCs".
        assert_eq!(
            nature_from_stat_change("Sp. Atk", "Defense"),
            Some(Nature::Mild)
        );
        assert_eq!(nature_from_stat_change("Speed", "Sp. Atk"), Some(Nature::Jolly));
        assert_eq!(nature_from_stat_change("Attack", "Sp. Atk"), Some(Nature::Adamant));

        // Every non-neutral nature round-trips through its own stat pair.
        let label = ["Attack", "Defense", "Sp. Atk", "Sp. Def", "Speed"];
        for nature in ALL_NATURES {
            let modifiers = crate::state::pokemon::nature_stat_modifiers(&nature);
            let up = modifiers.iter().position(|m| *m > 1.0);
            let down = modifiers.iter().position(|m| *m < 1.0);
            match (up, down) {
                (Some(u), Some(d)) => assert_eq!(
                    nature_from_stat_change(label[u], label[d]),
                    Some(nature),
                    "{nature:?} did not round-trip"
                ),
                // The five neutral natures have no stat pair to recover from.
                _ => assert!(matches!(
                    nature,
                    Nature::Hardy
                        | Nature::Docile
                        | Nature::Bashful
                        | Nature::Quirky
                        | Nature::Serious
                )),
            }
        }
    }

    #[test]
    fn neutral_and_malformed_stat_pairs_are_unrecoverable() {
        // The source writes neutral natures with matching stats; five natures
        // share that shape, so there is nothing to pick.
        assert_eq!(nature_from_stat_change("Attack", "Attack"), None);
        assert_eq!(nature_from_stat_change("Nonsense", "Defense"), None);
        assert_eq!(nature_from_stat_change("", ""), None);
    }

    #[test]
    fn all_natures_is_complete_and_resolvable() {
        let mut seen = std::collections::HashSet::new();
        for nature in ALL_NATURES {
            assert!(seen.insert(nature), "duplicate nature {nature:?}");
        }
        assert_eq!(ALL_NATURES.len(), 25);
        // Every entry round-trips through the resolver.
        for nature in ALL_NATURES {
            let name = format!("{nature:?}");
            assert_eq!(resolve_nature(&name), Some(nature));
        }
    }
}
