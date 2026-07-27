//! Deserialization of the raw championsbattledata.com API payload.
//!
//! `update_meta.py` writes the site's responses verbatim, so this mirrors their
//! shape rather than a shape of our choosing — and since re-running the scraper
//! can pick up whatever the site currently serves, everything here is built to
//! survive drift rather than to validate. Concretely:
//!
//! - every field is `#[serde(default)]`, so a *removed* column is not an error;
//! - both structs carry `#[serde(flatten)] extra`, so an *added* column is
//!   captured rather than rejected;
//! - `category` stays a `String` rather than an enum, so an unrecognized row
//!   kind can be counted and skipped instead of failing the whole file.
//!
//! The awkward part is that the payload is union-typed: every row object carries
//! all fourteen columns, and the ones that do not apply to its category hold the
//! empty string rather than `null` or nothing. So `hp_points` is `""` on a move
//! row and an integer on a `stat_points` row. `de_loose_*` absorb that.
//!
//! Numeric fields deserialize as `i64` and are clamped later, never as `u8`.
//! `state::pokemon::scale_evs_for_stat_points` casts through `as u8`, so a stat
//! point of 33 would silently become an EV of 4 instead of an obvious error;
//! keeping the wide type until the clamp is what makes that detectable.

use serde::{Deserialize, Deserializer};
use serde_json::{Map, Value};

/// One `<slug>.json` from the cache.
#[derive(Debug, Clone, Deserialize)]
pub struct MetaFile {
    #[serde(default)]
    pub pokemon: String,
    #[serde(default)]
    pub format: String,
    #[serde(default)]
    pub season: String,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub columns: Vec<String>,
    #[serde(default)]
    pub rows: Vec<MetaRow>,
    /// Anything the site adds that we do not model yet.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// One row of the per-Pokemon table.
#[derive(Debug, Clone, Deserialize)]
pub struct MetaRow {
    /// Constant within a file: a dense 1..=N usage rank, unique per format.
    /// (`meta_scraper/README.md` claims the API exposes no usage rank; this is
    /// one, though it is ordinal only — there is no absolute usage percentage
    /// anywhere in the payload.)
    #[serde(default)]
    pub column_position: Option<u32>,
    /// `move`, `held_item`, `teammate`, `stat_alignment`, `stat_points`, or
    /// `ability`. Deliberately not an enum — see the module docs.
    #[serde(default, deserialize_with = "de_loose_string")]
    pub category: String,
    /// 1-based rank within the category. Rows arrive in descending percentage
    /// order, which the null-imputation in `dex.rs` relies on.
    #[serde(default)]
    pub rank: Option<u32>,
    #[serde(default, deserialize_with = "de_loose_string")]
    pub name: String,
    /// Display form, e.g. `"89.1%"`. Prefer `percentage_value`; this is the
    /// fallback if the site ever drops the numeric field.
    #[serde(default, deserialize_with = "de_loose_string")]
    pub percentage: String,
    #[serde(default)]
    pub percentage_value: Option<f64>,
    /// Populated on `stat_alignment` rows, e.g. `"Speed"` / `"Sp. Atk"`.
    /// Neutral natures still fill these in.
    #[serde(default, deserialize_with = "de_loose_string")]
    pub stat_up: String,
    #[serde(default, deserialize_with = "de_loose_string")]
    pub stat_down: String,
    #[serde(default, deserialize_with = "de_loose_i64")]
    pub hp_points: Option<i64>,
    #[serde(default, deserialize_with = "de_loose_i64")]
    pub attack_points: Option<i64>,
    #[serde(default, deserialize_with = "de_loose_i64")]
    pub defense_points: Option<i64>,
    #[serde(default, deserialize_with = "de_loose_i64")]
    pub sp_atk_points: Option<i64>,
    #[serde(default, deserialize_with = "de_loose_i64")]
    pub sp_def_points: Option<i64>,
    #[serde(default, deserialize_with = "de_loose_i64")]
    pub speed_points: Option<i64>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl MetaRow {
    /// The row's percentage in 0..=100, or `None` when the source gives none.
    ///
    /// 19 non-teammate rows in the current cache are genuinely null (a named
    /// option with no recorded weight); every `teammate` row is null by design,
    /// as that category carries no percentages at all. Callers must impute or
    /// drop — never `unwrap`.
    pub fn pct(&self) -> Option<f64> {
        if let Some(v) = self.percentage_value
            && v.is_finite() {
                return Some(v);
            }
        let trimmed = self.percentage.trim().trim_end_matches('%').trim();
        if trimmed.is_empty() {
            return None;
        }
        trimmed.parse::<f64>().ok().filter(|v| v.is_finite())
    }

    /// The six stat-point fields, in `[hp, atk, def, spa, spd, spe]` order.
    ///
    /// `None` unless every one is present, which in practice means the row is a
    /// `stat_points` row. Values are raw and unclamped — the caller clamps to
    /// the authoring range 0..=32.
    pub fn raw_points(&self) -> Option<[i64; 6]> {
        Some([
            self.hp_points?,
            self.attack_points?,
            self.defense_points?,
            self.sp_atk_points?,
            self.sp_def_points?,
            self.speed_points?,
        ])
    }
}

/// Accepts a string, a number, `null`, or an absent field; yields `String`.
fn de_loose_string<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    Ok(match Option::<Value>::deserialize(d)? {
        Some(Value::String(s)) => s,
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::Bool(b)) => b.to_string(),
        _ => String::new(),
    })
}

/// Accepts an integer, a float, a numeric string, `""`, `null`, or an absent
/// field; yields `Option<i64>`. Non-numeric input is `None` rather than an
/// error, so one malformed cell cannot fail an otherwise-good file.
fn de_loose_i64<'de, D: Deserializer<'de>>(d: D) -> Result<Option<i64>, D::Error> {
    Ok(match Option::<Value>::deserialize(d)? {
        Some(Value::Number(n)) => n
            .as_i64()
            .or_else(|| n.as_f64().filter(|f| f.is_finite()).map(|f| f.round() as i64)),
        Some(Value::String(s)) => {
            let t = s.trim();
            if t.is_empty() {
                None
            } else {
                t.parse::<i64>().ok().or_else(|| {
                    t.parse::<f64>()
                        .ok()
                        .filter(|f| f.is_finite())
                        .map(|f| f.round() as i64)
                })
            }
        }
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A move row: the six point columns and both stat columns are `""`, which
    /// is the union-typing this module exists to absorb.
    const MOVE_ROW: &str = r#"{
        "pokemon":"Garchomp","column_position":1,"category":"move","rank":1,
        "name":"Dragon Claw","percentage":"89.1%","stat_up":"","stat_down":"",
        "hp_points":"","attack_points":"","defense_points":"","sp_atk_points":"",
        "sp_def_points":"","speed_points":"","percentage_value":89.1
    }"#;

    const SPREAD_ROW: &str = r#"{
        "pokemon":"Garchomp","column_position":1,"category":"stat_points","rank":1,
        "name":"","percentage":"47.3%","stat_up":"","stat_down":"",
        "hp_points":2,"attack_points":32,"defense_points":0,"sp_atk_points":0,
        "sp_def_points":0,"speed_points":32,"percentage_value":47.3
    }"#;

    #[test]
    fn parses_a_move_row_with_empty_numeric_columns() {
        let row: MetaRow = serde_json::from_str(MOVE_ROW).unwrap();
        assert_eq!(row.category, "move");
        assert_eq!(row.name, "Dragon Claw");
        assert_eq!(row.pct(), Some(89.1));
        assert_eq!(row.raw_points(), None);
        assert_eq!(row.column_position, Some(1));
    }

    #[test]
    fn parses_a_spread_row() {
        let row: MetaRow = serde_json::from_str(SPREAD_ROW).unwrap();
        assert_eq!(row.raw_points(), Some([2, 32, 0, 0, 0, 32]));
        assert_eq!(row.pct(), Some(47.3));
        // Spread rows are unnamed; the six point fields are the identity.
        assert!(row.name.is_empty());
    }

    #[test]
    fn null_percentage_is_none_not_a_panic() {
        let row: MetaRow = serde_json::from_str(
            r#"{"category":"held_item","name":"Mental Herb",
                "percentage":"","percentage_value":null}"#,
        )
        .unwrap();
        assert_eq!(row.pct(), None);
    }

    /// If the site ever drops `percentage_value`, the display string still
    /// carries the number.
    #[test]
    fn falls_back_to_the_display_percentage() {
        let row: MetaRow =
            serde_json::from_str(r#"{"category":"move","name":"Protect","percentage":"73.0%"}"#)
                .unwrap();
        assert_eq!(row.pct(), Some(73.0));
    }

    #[test]
    fn tolerates_added_and_removed_columns() {
        // Added: a column we have never seen. Removed: almost all of them.
        let row: MetaRow = serde_json::from_str(
            r#"{"category":"ability","name":"Rough Skin","percentage_value":97.6,
                "win_rate":0.51,"brand_new_column":{"nested":true}}"#,
        )
        .unwrap();
        assert_eq!(row.pct(), Some(97.6));
        assert!(row.extra.contains_key("win_rate"));
        assert!(row.extra.contains_key("brand_new_column"));
        assert_eq!(row.rank, None);
    }

    /// An unrecognized `category` must survive parsing so the loader can count
    /// and skip it; making it an enum would fail the entire file instead.
    #[test]
    fn unknown_category_is_not_an_error() {
        let row: MetaRow =
            serde_json::from_str(r#"{"category":"tera_type","name":"Fire","percentage_value":40.0}"#)
                .unwrap();
        assert_eq!(row.category, "tera_type");
    }

    #[test]
    fn numeric_strings_and_floats_both_parse() {
        let row: MetaRow = serde_json::from_str(
            r#"{"category":"stat_points","hp_points":"12","attack_points":31.0,
                "defense_points":0,"sp_atk_points":"0","sp_def_points":0,"speed_points":"23"}"#,
        )
        .unwrap();
        assert_eq!(row.raw_points(), Some([12, 31, 0, 0, 0, 23]));
    }

    /// Out-of-range values must arrive intact so the loader can clamp *and*
    /// warn. Deserializing straight into `u8` would wrap 260 to 4 with no signal.
    #[test]
    fn out_of_range_points_survive_deserialization() {
        let row: MetaRow = serde_json::from_str(
            r#"{"category":"stat_points","hp_points":260,"attack_points":-5,
                "defense_points":0,"sp_atk_points":0,"sp_def_points":0,"speed_points":0}"#,
        )
        .unwrap();
        assert_eq!(row.raw_points(), Some([260, -5, 0, 0, 0, 0]));
    }

    #[test]
    fn parses_a_whole_file_envelope() {
        let file: MetaFile = serde_json::from_str(&format!(
            r#"{{"pokemon":"Garchomp","format":"Doubles","season":"Season M-3",
                 "source":"x.csv","columns":["a","b"],"rows":[{MOVE_ROW},{SPREAD_ROW}]}}"#
        ))
        .unwrap();
        assert_eq!(file.pokemon, "Garchomp");
        assert_eq!(file.rows.len(), 2);
    }

    /// The scraper writes each response verbatim, so a truncated or renamed
    /// payload should degrade to an empty file rather than an error.
    #[test]
    fn missing_rows_yields_an_empty_file_not_an_error() {
        let file: MetaFile = serde_json::from_str(r#"{"pokemon":"Ditto"}"#).unwrap();
        assert!(file.rows.is_empty());
        assert!(file.season.is_empty());
    }
}
