//! Loads the resolved usage cache for sampling.
//!
//! An unknown species stops the load because it can produce incorrect stats.
//! Other unknown or invalid rows produce warnings and are removed.
//!
//! ## What the percentages mean
//!
//! Held items, abilities, natures, and stat spreads are truncated distributions.
//! Sampling normalizes the remaining permitted rows by their actual sum.
//!
//! Move rates are marginal inclusion rates and total about 400 percent.
//! Teammate rows use rank because they contain no percentages.
//!
//! Stat points use the teamsheet scale from zero through 32.
//! The loader checks this range before conversion.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::data::ability::Ability;
use crate::data::item::Item;
use crate::data::pokemon_move::PokemonMove;
use crate::data::species::Species;
use crate::meta::MetaFormat;
use crate::meta::names::{
    nature_from_stat_change,
    resolve_ability, resolve_item, resolve_move, resolve_nature, resolve_species,
};
use crate::meta::schema::{MetaFile, MetaRow};
use crate::state::pokemon::Nature;

/// Stat points in the authoring 0..=32 scale, `[hp, atk, def, spa, spd, spe]`.
pub type StatPoints = [u8; 6];

/// The largest legal value in a single stat-point slot.
pub const MAX_STAT_POINTS_PER_STAT: i64 = 32;

/// One option with the weight the site reports for it.
#[derive(Debug, Clone, PartialEq)]
pub struct Weighted<T> {
    pub value: T,
    /// Raw percentage in 0..=100 — not normalized, not divided by 100.
    pub pct: f64,
    /// 1-based rank within its category, as authored.
    pub rank: u32,
    /// `pct` was imputed because the source gave none.
    pub imputed: bool,
}

/// Everything the cache knows about one species in one format.
#[derive(Debug, Clone)]
pub struct SpeciesMeta {
    pub species: Species,
    /// The file's `column_position`: a dense 1..=N usage rank, unique per
    /// format. Ordinal only — there is no absolute usage percentage in the
    /// payload. `u32::MAX` when the file omitted it.
    pub usage_rank: u32,
    /// Marginal inclusion rates, NOT a distribution. See the module docs.
    pub moves: Vec<Weighted<PokemonMove>>,
    pub items: Vec<Weighted<Item>>,
    pub abilities: Vec<Weighted<Ability>>,
    pub natures: Vec<Weighted<Nature>>,
    pub spreads: Vec<Weighted<StatPoints>>,
    /// `pct` is always 0.0 here; use `MetaDex::teammate_score`.
    pub teammates: Vec<Weighted<Species>>,
}

/// A recoverable problem found while loading. Collected rather than returned so
/// one bad row cannot deny the caller 234 good species.
#[derive(Debug, Clone, PartialEq)]
pub enum MetaWarning {
    UnmappedName {
        species: Species,
        category: &'static str,
        raw: String,
    },
    ImputedPercentage {
        species: Species,
        category: &'static str,
        raw: String,
        imputed: f64,
    },
    EmptyName {
        species: Species,
        category: &'static str,
    },
    ClampedStatPoint {
        species: Species,
        stat: usize,
        raw: i64,
    },
    UnknownCategory {
        species: Species,
        category: String,
    },
    SpeciesFileSkipped {
        file: String,
        reason: String,
    },
    SummaryMissing {
        path: PathBuf,
    },
}

#[derive(Debug)]
pub enum MetaError {
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    Json {
        path: PathBuf,
        source: serde_json::Error,
    },
    /// A species name in the cache does not resolve to a real `Species`.
    /// Deliberately fatal: `Species::from_str` would have returned
    /// `Species::Unknown`, and `build_pokemon_state` gives that `[100; 6]` base
    /// stats — a plausible-looking Pokemon that is wrong everywhere.
    UnresolvableSpecies {
        file: PathBuf,
        raw: String,
    },
    NoSeasonFound {
        root: PathBuf,
    },
    FormatMissing {
        root: PathBuf,
        format: MetaFormat,
    },
}

impl std::fmt::Display for MetaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MetaError::Io { path, source } => write!(f, "reading {}: {source}", path.display()),
            MetaError::Json { path, source } => write!(f, "parsing {}: {source}", path.display()),
            MetaError::UnresolvableSpecies { file, raw } => write!(
                f,
                "{}: species name {raw:?} does not map to a known Species — add it to \
                 meta::names::SPECIES_OVERRIDES rather than letting it become Species::Unknown",
                file.display()
            ),
            MetaError::NoSeasonFound { root } => {
                write!(f, "no season directory found under {}", root.display())
            }
            MetaError::FormatMissing { root, format } => write!(
                f,
                "{} has no {} directory",
                root.display(),
                format.dir_name()
            ),
        }
    }
}

impl std::error::Error for MetaError {}

pub struct MetaDex {
    season: String,
    format: MetaFormat,
    by_species: HashMap<Species, SpeciesMeta>,
    item_pool: HashSet<Item>,
    /// `_summary.json`'s `teammateAppearanceCounts`: how often each species
    /// appears as somebody else's listed teammate. The scraper's own
    /// aggregation, and the closest thing to a usage prior the data affords.
    teammate_counts: HashMap<Species, u32>,
    max_teammate_count: f64,
    warnings: Vec<MetaWarning>,
}

impl MetaDex {
    /// Load one season/format pair.
    ///
    /// `root` is the `meta_scraper/data` directory. `season = None` discovers the
    /// season from `index.json`, falling back to the lexicographically greatest
    /// subdirectory. Never hardcode a season name: `update_meta.py` names its
    /// output directory after whatever season the API echoes back, so a refresh
    /// after the site rolls over writes a *new* folder beside the old one and
    /// repoints `index.json`. A hardcoded name would silently keep reading stale
    /// data.
    pub fn load(
        root: &Path,
        season: Option<&str>,
        format: MetaFormat,
    ) -> Result<MetaDex, MetaError> {
        let season = discover_season(root, season)?;
        let dir = root.join(&season).join(format.dir_name());
        if !dir.is_dir() {
            return Err(MetaError::FormatMissing {
                root: root.join(&season),
                format,
            });
        }

        let mut warnings = Vec::new();
        let mut by_species: HashMap<Species, SpeciesMeta> = HashMap::new();
        let mut item_pool = HashSet::new();

        let entries = std::fs::read_dir(&dir).map_err(|source| MetaError::Io {
            path: dir.clone(),
            source,
        })?;
        // Sort for deterministic warning order across platforms.
        let mut paths: Vec<PathBuf> = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|source| MetaError::Io {
                path: dir.clone(),
                source,
            })?;
            let path = entry.path();
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            // `_summary.json` is metadata, not a species.
            if name.starts_with('_') || !name.ends_with(".json") {
                continue;
            }
            paths.push(path);
        }
        paths.sort();

        for path in &paths {
            let text = std::fs::read_to_string(path).map_err(|source| MetaError::Io {
                path: path.clone(),
                source,
            })?;
            let file: MetaFile =
                serde_json::from_str(&text).map_err(|source| MetaError::Json {
                    path: path.clone(),
                    source,
                })?;

            let slug = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default()
                .to_string();
            // Prefer the payload's display name; fall back to the file slug.
            // Both normalize to the same key for every name in the cache, so
            // this only matters if the site starts disagreeing with itself.
            let species = resolve_species(&file.pokemon)
                .or_else(|| resolve_species(&slug))
                .ok_or_else(|| MetaError::UnresolvableSpecies {
                    file: path.clone(),
                    raw: if file.pokemon.is_empty() {
                        slug.clone()
                    } else {
                        file.pokemon.clone()
                    },
                })?;

            let meta = parse_species(&species, &file, &mut warnings);
            item_pool.extend(meta.items.iter().map(|w| w.value.clone()));
            by_species.insert(species, meta);
        }

        let (teammate_counts, max_teammate_count) = load_summary(&dir, &mut warnings);

        Ok(MetaDex {
            season,
            format,
            by_species,
            item_pool,
            teammate_counts,
            max_teammate_count,
            warnings,
        })
    }

    pub fn season(&self) -> &str {
        &self.season
    }

    pub fn format(&self) -> MetaFormat {
        self.format
    }

    pub fn get(&self, species: &Species) -> Option<&SpeciesMeta> {
        self.by_species.get(species)
    }

    pub fn len(&self) -> usize {
        self.by_species.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_species.is_empty()
    }

    pub fn species(&self) -> impl Iterator<Item = &Species> {
        self.by_species.keys()
    }

    pub fn warnings(&self) -> &[MetaWarning] {
        &self.warnings
    }

    /// Every item that appears on any species in this format.
    ///
    /// This is the determinizer's fallback item domain: a pool that is legal by
    /// construction (someone actually ran each of these) and available without
    /// enumerating the ~1,000-variant `Item` enum, most of which is unobtainable.
    pub fn item_pool(&self) -> &HashSet<Item> {
        &self.item_pool
    }

    /// How strongly `candidate` co-occurs with `known`.
    ///
    /// Teammate rows carry no percentages, so rank is all there is: rank 1 scores
    /// 1.0, rank 10 scores 0.1. Returns 0.0 when the pair never co-occurs, so a
    /// caller summing this over several known mons gets a usable ordering
    /// without special-casing absence.
    pub fn teammate_score(&self, known: &Species, candidate: &Species) -> f64 {
        let Some(meta) = self.by_species.get(known) else {
            return 0.0;
        };
        meta.teammates
            .iter()
            .find(|w| &w.value == candidate)
            .map(|w| 1.0 / w.rank.max(1) as f64)
            .unwrap_or(0.0)
    }

    /// Format-wide popularity prior in roughly 0..=1.
    ///
    /// Derived from `teammateAppearanceCounts` when available — an approximation
    /// the scraper documents as such — falling back to `1 / usage_rank`, which is
    /// ordinal but at least monotone in real usage.
    pub fn popularity(&self, species: &Species) -> f64 {
        if self.max_teammate_count > 0.0
            && let Some(count) = self.teammate_counts.get(species) {
                return *count as f64 / self.max_teammate_count;
            }
        self.by_species
            .get(species)
            .map(|m| 1.0 / m.usage_rank.max(1) as f64)
            .unwrap_or(0.0)
    }
}

/// Resolve which season directory to read.
fn discover_season(root: &Path, requested: Option<&str>) -> Result<String, MetaError> {
    if let Some(season) = requested {
        return Ok(season.to_string());
    }
    if let Ok(text) = std::fs::read_to_string(root.join("index.json"))
        && let Ok(value) = serde_json::from_str::<serde_json::Value>(&text)
            && let Some(season) = value.get("season").and_then(|s| s.as_str())
                && root.join(season).is_dir() {
                    return Ok(season.to_string());
                }
    // `update_meta.py` never deletes an old season, so prefer the greatest name.
    let mut seasons: Vec<String> = std::fs::read_dir(root)
        .map_err(|source| MetaError::Io {
            path: root.to_path_buf(),
            source,
        })?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .collect();
    seasons.sort();
    seasons.pop().ok_or_else(|| MetaError::NoSeasonFound {
        root: root.to_path_buf(),
    })
}

/// Read `_summary.json` for the teammate-appearance prior.
fn load_summary(dir: &Path, warnings: &mut Vec<MetaWarning>) -> (HashMap<Species, u32>, f64) {
    let path = dir.join("_summary.json");
    let Ok(text) = std::fs::read_to_string(&path) else {
        warnings.push(MetaWarning::SummaryMissing { path });
        return (HashMap::new(), 0.0);
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        warnings.push(MetaWarning::SummaryMissing { path });
        return (HashMap::new(), 0.0);
    };
    let mut counts = HashMap::new();
    let mut max = 0.0f64;
    if let Some(map) = value.get("teammateAppearanceCounts").and_then(|v| v.as_object()) {
        for (name, count) in map {
            let Some(count) = count.as_u64() else { continue };
            // An unresolvable name here is not fatal: this feeds a tie-breaking
            // prior, not a Pokemon that gets built.
            if let Some(species) = resolve_species(name) {
                counts.insert(species, count as u32);
                max = max.max(count as f64);
            }
        }
    }
    (counts, max)
}

/// Turn one file's rows into a `SpeciesMeta`.
fn parse_species(
    species: &Species,
    file: &MetaFile,
    warnings: &mut Vec<MetaWarning>,
) -> SpeciesMeta {
    let usage_rank = file
        .rows
        .iter()
        .find_map(|r| r.column_position)
        .unwrap_or(u32::MAX);

    // Every category is optional: `ditto` has no moves, `gourgeist` and
    // `mudsdale` no abilities, `decidueye` and `toucannon` no natures.
    let meta = SpeciesMeta {
        species: species.clone(),
        usage_rank,
        moves: collect(species, file, "move", warnings, resolve_move),
        items: collect(species, file, "held_item", warnings, resolve_item),
        abilities: collect(species, file, "ability", warnings, resolve_ability),
        natures: collect_natures(species, file, warnings),
        spreads: collect_spreads(species, file, warnings),
        teammates: collect_teammates(species, file, warnings),
    };

    for row in &file.rows {
        if !matches!(
            row.category.as_str(),
            "move" | "held_item" | "ability" | "stat_alignment" | "stat_points" | "teammate"
        ) {
            warnings.push(MetaWarning::UnknownCategory {
                species: species.clone(),
                category: row.category.clone(),
            });
        }
    }

    meta
}

/// Rows of one category, in rank order.
fn rows_of<'a>(file: &'a MetaFile, category: &str) -> Vec<&'a MetaRow> {
    let mut rows: Vec<&MetaRow> = file
        .rows
        .iter()
        .filter(|r| r.category == category)
        .collect();
    // Imputation walks neighbours, so rank order must hold even if the payload
    // ever stops emitting rows pre-sorted.
    rows.sort_by_key(|r| r.rank.unwrap_or(u32::MAX));
    rows
}

/// Fill in missing percentages from their rank neighbours.
///
/// Rows are ordered by descending percentage, so a null is bracketed by its
/// neighbours and the geometric mean respects that ordering. Tail rows (nothing
/// below) get half the last known value, floored well above zero so the option
/// stays reachable.
fn impute_percentages(rows: &[&MetaRow]) -> Vec<(f64, bool)> {
    let raw: Vec<Option<f64>> = rows.iter().map(|r| r.pct()).collect();
    raw.iter()
        .enumerate()
        .map(|(i, cur)| match cur {
            Some(v) => (*v, false),
            None => {
                let above = raw[..i].iter().rev().flatten().next().copied();
                let below = raw[i + 1..].iter().flatten().next().copied();
                let value = match (above, below) {
                    (Some(a), Some(b)) => (a.max(0.0) * b.max(0.0)).sqrt(),
                    (Some(a), None) => (a * 0.5).max(0.05),
                    (None, Some(b)) => b.max(0.0),
                    (None, None) => 0.05,
                };
                (value, true)
            }
        })
        .collect()
}

/// Named categories: move / held_item / ability / stat_alignment.
fn collect<T>(
    species: &Species,
    file: &MetaFile,
    category: &'static str,
    warnings: &mut Vec<MetaWarning>,
    resolve: impl Fn(&str) -> Option<T>,
) -> Vec<Weighted<T>> {
    let rows = rows_of(file, category);
    let pcts = impute_percentages(&rows);
    let mut out = Vec::with_capacity(rows.len());

    for (idx, row) in rows.iter().enumerate() {
        if row.name.trim().is_empty() {
            warnings.push(MetaWarning::EmptyName {
                species: species.clone(),
                category,
            });
            continue;
        }
        let Some(value) = resolve(&row.name) else {
            warnings.push(MetaWarning::UnmappedName {
                species: species.clone(),
                category,
                raw: row.name.clone(),
            });
            continue;
        };
        let (pct, imputed) = pcts[idx];
        if imputed {
            warnings.push(MetaWarning::ImputedPercentage {
                species: species.clone(),
                category,
                raw: row.name.clone(),
                imputed: pct,
            });
        }
        out.push(Weighted {
            value,
            pct: pct.max(0.0),
            rank: row.rank.unwrap_or(idx as u32 + 1),
            imputed,
        });
    }
    out
}

/// Nature rows, falling back to the raised/lowered stat pair when the name is
/// unusable.
///
/// The source occasionally corrupts a nature's name while leaving `stat_up` and
/// `stat_down` intact, and those two determine the nature outright — so the row
/// is repairable rather than droppable. Only a name that resolves *neither* way
/// becomes an `UnmappedName` warning.
fn collect_natures(
    species: &Species,
    file: &MetaFile,
    warnings: &mut Vec<MetaWarning>,
) -> Vec<Weighted<Nature>> {
    let rows = rows_of(file, "stat_alignment");
    let pcts = impute_percentages(&rows);
    let mut out = Vec::with_capacity(rows.len());

    for (idx, row) in rows.iter().enumerate() {
        let resolved = resolve_nature(&row.name)
            .or_else(|| nature_from_stat_change(&row.stat_up, &row.stat_down));
        let Some(value) = resolved else {
            warnings.push(MetaWarning::UnmappedName {
                species: species.clone(),
                category: "stat_alignment",
                raw: row.name.clone(),
            });
            continue;
        };
        let (pct, imputed) = pcts[idx];
        if imputed {
            warnings.push(MetaWarning::ImputedPercentage {
                species: species.clone(),
                category: "stat_alignment",
                raw: row.name.clone(),
                imputed: pct,
            });
        }
        out.push(Weighted {
            value,
            pct: pct.max(0.0),
            rank: row.rank.unwrap_or(idx as u32 + 1),
            imputed,
        });
    }
    out
}

/// `stat_points` rows are unnamed — the six point fields are the identity.
fn collect_spreads(
    species: &Species,
    file: &MetaFile,
    warnings: &mut Vec<MetaWarning>,
) -> Vec<Weighted<StatPoints>> {
    let rows = rows_of(file, "stat_points");
    let pcts = impute_percentages(&rows);
    let mut out = Vec::with_capacity(rows.len());

    for (idx, row) in rows.iter().enumerate() {
        let Some(raw_points) = row.raw_points() else {
            continue;
        };
        let mut points = [0u8; 6];
        for (stat, raw) in raw_points.iter().enumerate() {
            // Clamp rather than cast: `scale_evs_for_stat_points` goes through
            // `as u8`, so 33 points would become 4 EVs with no diagnostic.
            let clamped = (*raw).clamp(0, MAX_STAT_POINTS_PER_STAT);
            if clamped != *raw {
                warnings.push(MetaWarning::ClampedStatPoint {
                    species: species.clone(),
                    stat,
                    raw: *raw,
                });
            }
            points[stat] = clamped as u8;
        }
        let (pct, imputed) = pcts[idx];
        if imputed {
            warnings.push(MetaWarning::ImputedPercentage {
                species: species.clone(),
                category: "stat_points",
                raw: format!("{points:?}"),
                imputed: pct,
            });
        }
        out.push(Weighted {
            value: points,
            pct: pct.max(0.0),
            rank: row.rank.unwrap_or(idx as u32 + 1),
            imputed,
        });
    }
    out
}

/// Teammates never carry percentages, so `pct` stays 0.0 and rank is the signal.
/// Imputing here would invent co-occurrence weights the source does not have.
fn collect_teammates(
    species: &Species,
    file: &MetaFile,
    warnings: &mut Vec<MetaWarning>,
) -> Vec<Weighted<Species>> {
    let mut out = Vec::new();
    for (idx, row) in rows_of(file, "teammate").iter().enumerate() {
        if row.name.trim().is_empty() {
            continue;
        }
        let Some(value) = resolve_species(&row.name) else {
            warnings.push(MetaWarning::UnmappedName {
                species: species.clone(),
                category: "teammate",
                raw: row.name.clone(),
            });
            continue;
        };
        out.push(Weighted {
            value,
            pct: 0.0,
            rank: row.rank.unwrap_or(idx as u32 + 1),
            imputed: false,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The cache is gitignored and regenerable, so it may simply be absent —
    /// skip rather than fail, or a fresh clone cannot run the suite.
    fn data_root() -> Option<PathBuf> {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../meta_scraper/data");
        root.is_dir().then_some(root)
    }

    fn count<F: Fn(&MetaWarning) -> bool>(dex: &MetaDex, pred: F) -> usize {
        dex.warnings().iter().filter(|w| pred(w)).count()
    }

    /// The load-bearing sweep: every file in the cache, both formats.
    ///
    /// The assertions here are invariants, not a snapshot. The cache is
    /// regenerable and its *contents* legitimately change whenever
    /// `update_meta.py` runs against a new season — percentages move, options
    /// enter and leave the top-N lists, whole categories appear. Asserting those
    /// would mean a test that fails on every refresh for no real reason.
    ///
    /// What must never change is that every name resolves, every category is
    /// understood, and every stat point is in range. The strongest tripwire is
    /// not even here: an unresolvable species is a hard `MetaError` from
    /// `MetaDex::load`, which is what caught the site renaming `Rotom Fan` to
    /// `Fan Rotom` on the first refresh after this module was written.
    #[test]
    fn loads_the_entire_cache() {
        let Some(root) = data_root() else { return };

        for format in [MetaFormat::Doubles, MetaFormat::Singles] {
            let dex = MetaDex::load(&root, None, format)
                .unwrap_or_else(|e| panic!("{format:?} failed to load: {e}"));

            assert_eq!(dex.len(), 235, "{format:?} species count");

            // An unmapped name means SPECIES_OVERRIDES or an enum has drifted.
            assert_eq!(
                count(&dex, |w| matches!(w, MetaWarning::UnmappedName { .. })),
                0,
                "{format:?} unmapped names: {:?}",
                dex.warnings()
                    .iter()
                    .filter(|w| matches!(w, MetaWarning::UnmappedName { .. }))
                    .collect::<Vec<_>>()
            );
            assert_eq!(
                count(&dex, |w| matches!(w, MetaWarning::UnknownCategory { .. })),
                0,
                "{format:?} unknown categories"
            );
            // No authored spread should ever exceed the 0..=32 range.
            assert_eq!(
                count(&dex, |w| matches!(w, MetaWarning::ClampedStatPoint { .. })),
                0,
                "{format:?} clamped stat points"
            );
            // Imputation and empty names track how tidy the source happens to
            // be — the Season M-3 cache had 19 null percentages and one unnamed
            // move row, the refresh that followed had none. Both are handled
            // either way; a *large* number would mean something structural
            // broke, so this is a sanity bound rather than a snapshot.
            let imputed = count(&dex, |w| matches!(w, MetaWarning::ImputedPercentage { .. }));
            let empty = count(&dex, |w| matches!(w, MetaWarning::EmptyName { .. }));
            assert!(
                imputed < 200,
                "{format:?}: {imputed} imputed percentages — the source shape has changed"
            );
            assert!(
                empty < 200,
                "{format:?}: {empty} empty names — the source shape has changed"
            );
        }
    }

    /// Guards the invariant the whole stat-point pipeline rests on: these are
    /// authoring units, 0..=32, not the 0-252 EV scale.
    #[test]
    fn every_spread_is_in_authoring_units() {
        let Some(root) = data_root() else { return };
        for format in [MetaFormat::Doubles, MetaFormat::Singles] {
            let dex = MetaDex::load(&root, None, format).unwrap();
            let mut saw_a_full_budget = false;
            for species in dex.species() {
                for spread in &dex.get(species).unwrap().spreads {
                    let total: u32 = spread.value.iter().map(|p| *p as u32).sum();
                    assert!(
                        spread.value.iter().all(|p| *p as i64 <= MAX_STAT_POINTS_PER_STAT),
                        "{species:?} spread {:?} exceeds the per-stat cap",
                        spread.value
                    );
                    assert!(total <= 66, "{species:?} spread {:?} totals {total}", spread.value);
                    saw_a_full_budget |= total == 66;
                }
            }
            assert!(saw_a_full_budget, "{format:?}: no spread spends the full 66 points");
        }
    }

    /// Percentages are raw site values, so they neither sum to 1 nor reliably to
    /// 100. Anything that renormalizes must divide by the actual sum.
    #[test]
    fn percentages_are_unnormalized_and_may_exceed_100() {
        let Some(root) = data_root() else { return };
        let dex = MetaDex::load(&root, None, MetaFormat::Doubles).unwrap();

        let chomp = dex.get(&Species::Garchomp).expect("Garchomp is in the cache");
        // Moves are marginal inclusion rates: ~4 slots x 100%, not a distribution.
        let move_sum: f64 = chomp.moves.iter().map(|w| w.pct).sum();
        assert!(
            (300.0..400.0).contains(&move_sum),
            "Garchomp move sum {move_sum} is not ~4 slots"
        );
        // Items are a truncated distribution: under 100, remainder is "other".
        let item_sum: f64 = chomp.items.iter().map(|w| w.pct).sum();
        assert!(item_sum < 100.0, "Garchomp item sum {item_sum}");

        // Somewhere in the cache a category tops 100 from the site's rounding.
        let any_over_100 = dex.species().any(|s| {
            let m = dex.get(s).unwrap();
            m.natures.iter().map(|w| w.pct).sum::<f64>() > 100.0
        });
        assert!(any_over_100, "expected at least one nature sum above 100");
    }

    /// Parser fidelity, checked against the raw JSON rather than against
    /// remembered numbers.
    ///
    /// The percentages in the cache change every time the scraper runs, so
    /// hardcoding them would test the season rather than the parser. Comparing
    /// the loaded `SpeciesMeta` back to the file it came from tests the thing
    /// that must actually hold, and keeps holding after a refresh.
    #[test]
    fn parses_a_species_file_faithfully() {
        let Some(root) = data_root() else { return };
        let dex = MetaDex::load(&root, None, MetaFormat::Doubles).unwrap();
        let chomp = dex.get(&Species::Garchomp).expect("Garchomp is in the cache");

        let raw: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(
                root.join(dex.season())
                    .join("Doubles")
                    .join("garchomp.json"),
            )
            .unwrap(),
        )
        .unwrap();
        let rows = raw["rows"].as_array().unwrap();
        let of_category = |category: &str| -> Vec<&serde_json::Value> {
            rows.iter().filter(|r| r["category"] == category).collect()
        };

        assert_eq!(
            chomp.usage_rank,
            rows[0]["column_position"].as_u64().unwrap() as u32
        );

        // Every named category round-trips name-for-name and value-for-value.
        for (category, loaded) in [
            ("move", chomp.moves.iter().map(|w| w.pct).collect::<Vec<_>>()),
            ("held_item", chomp.items.iter().map(|w| w.pct).collect()),
            ("ability", chomp.abilities.iter().map(|w| w.pct).collect()),
            ("stat_alignment", chomp.natures.iter().map(|w| w.pct).collect()),
        ] {
            let expected: Vec<f64> = of_category(category)
                .iter()
                .map(|r| r["percentage_value"].as_f64().unwrap_or(0.0))
                .collect();
            assert_eq!(loaded, expected, "{category} percentages");
        }

        assert_eq!(
            chomp.moves[0].value,
            crate::meta::names::resolve_move(of_category("move")[0]["name"].as_str().unwrap())
                .unwrap()
        );

        // Spread rows are unnamed; the six point fields are the identity.
        let first_spread = of_category("stat_points")[0];
        let expected_points: Vec<u8> = [
            "hp_points",
            "attack_points",
            "defense_points",
            "sp_atk_points",
            "sp_def_points",
            "speed_points",
        ]
        .iter()
        .map(|k| first_spread[k].as_u64().unwrap() as u8)
        .collect();
        assert_eq!(chomp.spreads[0].value.to_vec(), expected_points);

        // Ranks are ascending and percentages descending, which the
        // null-imputation's neighbour walk depends on.
        assert!(chomp.moves.windows(2).all(|w| w[0].rank <= w[1].rank));
        assert!(chomp.items.windows(2).all(|w| w[0].pct >= w[1].pct));

        assert!(!chomp.teammates.is_empty());
        assert!(
            chomp.teammates.iter().all(|t| t.pct == 0.0),
            "teammate rows carry no percentages"
        );
    }

    /// Every category is optional, and which species are missing which changes
    /// between seasons — Season M-3 had `gourgeist` with no abilities and
    /// `decidueye` with no natures; the next refresh had neither gap but still
    /// had `ditto` with no moves. So this asserts the loader *handles* a missing
    /// category, not which species happens to have one.
    #[test]
    fn tolerates_species_with_missing_categories() {
        let Some(root) = data_root() else { return };
        let dex = MetaDex::load(&root, None, MetaFormat::Doubles).unwrap();

        // Ditto has never had move data: it only ever uses Transform.
        assert!(
            dex.get(&Species::Ditto).unwrap().moves.is_empty(),
            "Ditto is the standing example of a category-less species"
        );

        // Whatever else is missing, a gap must be an empty vec rather than a
        // panic or a phantom entry.
        for species in dex.species() {
            let meta = dex.get(species).unwrap();
            for spread in &meta.spreads {
                assert!(spread.pct >= 0.0, "{species:?} has a negative weight");
            }
            assert!(
                meta.abilities.len() <= 4,
                "{species:?} has {} abilities",
                meta.abilities.len()
            );
        }
    }

    #[test]
    fn resolves_the_forme_species() {
        let Some(root) = data_root() else { return };
        let dex = MetaDex::load(&root, None, MetaFormat::Doubles).unwrap();
        // The irregular mappings must actually land in the dex, not just resolve.
        for species in [
            Species::GourgeistSuper,
            Species::ZoroarkHisui,
            Species::TaurosPaldeaAqua,
            Species::NinetalesAlola,
            Species::BasculegionF,
        ] {
            assert!(dex.get(&species).is_some(), "{species:?} missing from dex");
        }
    }

    #[test]
    fn item_pool_is_a_usable_fallback_domain() {
        let Some(root) = data_root() else { return };
        let dex = MetaDex::load(&root, None, MetaFormat::Doubles).unwrap();
        let pool = dex.item_pool();
        assert!(pool.len() > 100, "item pool is only {}", pool.len());
        assert!(pool.contains(&Item::LifeOrb));
        // Never a phantom: `resolve_item` rejects `Item::Unknown`.
        assert!(!pool.iter().any(|i| matches!(i, Item::Unknown(_))));
    }

    #[test]
    fn teammate_and_popularity_priors_are_ordered() {
        let Some(root) = data_root() else { return };
        let dex = MetaDex::load(&root, None, MetaFormat::Doubles).unwrap();

        let chomp = dex.get(&Species::Garchomp).unwrap();
        let first = chomp.teammates[0].value.clone();
        let last = chomp.teammates.last().unwrap().value.clone();
        assert!(
            dex.teammate_score(&Species::Garchomp, &first)
                > dex.teammate_score(&Species::Garchomp, &last),
            "rank 1 should outscore the last teammate"
        );
        // A pair that never co-occurs scores zero rather than panicking.
        assert_eq!(dex.teammate_score(&Species::Garchomp, &Species::Ababo), 0.0);

        // Popularity is a real prior, in range, and orders common above rare.
        assert!(dex.popularity(&Species::Garchomp) > 0.0);
        assert!(dex.species().all(|s| (0.0..=1.0).contains(&dex.popularity(s))));
    }

    /// Season discovery must come from `index.json`, never a hardcoded name:
    /// re-running the scraper after a season rollover writes a new directory
    /// beside the old one and repoints the index.
    #[test]
    fn discovers_the_season_from_the_index() {
        let Some(root) = data_root() else { return };
        let dex = MetaDex::load(&root, None, MetaFormat::Doubles).unwrap();
        let index: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(root.join("index.json")).unwrap())
                .unwrap();
        assert_eq!(dex.season(), index["season"].as_str().unwrap());
    }

    #[test]
    fn imputation_uses_the_rank_neighbours() {
        // Bracketed by neighbours -> geometric mean, and strictly between them.
        let file: MetaFile = serde_json::from_str(
            r#"{"rows":[
                {"category":"held_item","rank":1,"name":"A","percentage_value":80.0},
                {"category":"held_item","rank":2,"name":"B","percentage":"","percentage_value":null},
                {"category":"held_item","rank":3,"name":"C","percentage_value":20.0}]}"#,
        )
        .unwrap();
        let mut warnings = Vec::new();
        let items = collect(&Species::Garchomp, &file, "held_item", &mut warnings, |n| {
            Some(n.to_string())
        });
        assert!((items[1].pct - 40.0).abs() < 1e-9, "got {}", items[1].pct);
        assert!(items[1].pct < items[0].pct && items[1].pct > items[2].pct);
        assert!(items[1].imputed);
        assert_eq!(warnings.len(), 1);
    }

    #[test]
    fn imputation_of_a_tail_row_stays_positive() {
        let file: MetaFile = serde_json::from_str(
            r#"{"rows":[
                {"category":"held_item","rank":1,"name":"A","percentage_value":6.0},
                {"category":"held_item","rank":2,"name":"B","percentage_value":null}]}"#,
        )
        .unwrap();
        let mut warnings = Vec::new();
        let items = collect(&Species::Garchomp, &file, "held_item", &mut warnings, |n| {
            Some(n.to_string())
        });
        // Half the last known value, and never zero — the option stays reachable.
        assert!((items[1].pct - 3.0).abs() < 1e-9);
        assert!(items[1].pct > 0.0);
    }

    /// A stray out-of-range point must be clamped *and* reported, never cast.
    #[test]
    fn clamps_and_warns_on_out_of_range_points() {
        let file: MetaFile = serde_json::from_str(
            r#"{"rows":[{"category":"stat_points","rank":1,"percentage_value":50.0,
                "hp_points":260,"attack_points":-5,"defense_points":0,
                "sp_atk_points":0,"sp_def_points":0,"speed_points":0}]}"#,
        )
        .unwrap();
        let mut warnings = Vec::new();
        let spreads = collect_spreads(&Species::Garchomp, &file, &mut warnings);
        // 260 would have become 4 under a plain `as u8`.
        assert_eq!(spreads[0].value, [32, 0, 0, 0, 0, 0]);
        assert_eq!(
            warnings
                .iter()
                .filter(|w| matches!(w, MetaWarning::ClampedStatPoint { .. }))
                .count(),
            2
        );
    }

    /// A nature row whose name is corrupt is repaired from its stat pair rather
    /// than dropped — the pair identifies the nature outright.
    #[test]
    fn recovers_a_nature_row_with_a_corrupt_name() {
        let file: MetaFile = serde_json::from_str(
            r#"{"rows":[{"category":"stat_alignment","rank":1,"name":"SCs",
                "percentage_value":0.7,"stat_up":"Sp. Atk","stat_down":"Defense"}]}"#,
        )
        .unwrap();
        let mut warnings = Vec::new();
        let natures = collect_natures(&Species::Stunfisk, &file, &mut warnings);
        assert_eq!(natures.len(), 1, "the row should be recovered, not dropped");
        assert_eq!(natures[0].value, Nature::Mild);
        assert!((natures[0].pct - 0.7).abs() < 1e-9);
        assert!(warnings.is_empty(), "recovery should not warn: {warnings:?}");
    }

    /// A name that resolves neither way is still a warning, so genuine drift
    /// stays visible.
    #[test]
    fn an_irrecoverable_nature_row_still_warns() {
        let file: MetaFile = serde_json::from_str(
            r#"{"rows":[{"category":"stat_alignment","rank":1,"name":"???",
                "percentage_value":1.0,"stat_up":"","stat_down":""}]}"#,
        )
        .unwrap();
        let mut warnings = Vec::new();
        let natures = collect_natures(&Species::Stunfisk, &file, &mut warnings);
        assert!(natures.is_empty());
        assert_eq!(warnings.len(), 1);
        assert!(matches!(warnings[0], MetaWarning::UnmappedName { .. }));
    }

    #[test]
    fn unresolvable_species_is_a_hard_error() {
        // Simulates the site adding a forme we have no override for.
        assert!(resolve_species("Kalosian Garchomp").is_none());
    }
}
