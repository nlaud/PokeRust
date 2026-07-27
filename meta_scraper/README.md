# meta_scraper

Local cache of Pokémon Champions competitive usage stats — common Pokémon, common
partners (teammates), items, moves, abilities, and EV/nature spreads — scraped from
**[Pokemon Champions Battle Data](https://championsbattledata.com/)**, sourced via
that site's own documented, no-auth JSON API rather than screen-scraping.

This is the data-collection half of `TODO.md`'s longstanding "meta sampler" feature
(originally scoped against pikalytics, but championsbattledata.com is the actual
usage-stats site for this game). The consuming half now exists:
`poke_rust/src/meta/` parses this cache, and
`poke_rust/src/information/determinize.rs` samples concrete opponent teams from it
to turn a fog-of-war belief into a playable `BattleState`. The eventual consumers
beyond that are the planned meta-aware bot/Nash solver (see `TODO.md`).

## Attribution (required)

Per the site's license, whenever this data is shared or used elsewhere, credit
**Pokemon Champions Battle Data** and link to <https://championsbattledata.com/>.
This is a fan-made project, not affiliated with Pokémon, Nintendo, Game Freak, or
Creatures Inc.

## Usage

Update everything (current season, both Singles and Doubles, the full roster):

```sh
python meta_scraper/update_meta.py
```

(On Windows, `py meta_scraper/update_meta.py` works too if `python` isn't on PATH.)

Pure Python 3 standard library — no `pip install` needed, no dependency manifest.
Takes a few minutes; progress is printed as it runs.

Narrower re-runs, e.g. while debugging:

```sh
python meta_scraper/update_meta.py --format Doubles --pokemon garchomp --pokemon tyranitar
```

Re-running always refreshes in place — this is a cache, not an archive; there's no
"undo," it just reflects whatever the site currently reports.

## On-disk layout (`meta_scraper/data/`, gitignored)

```
data/
  index.json                          # roster size, resolved season, generation time
  <Season>/<Format>/<pokemon-slug>.json   # one file per Pokémon+format, the site's
                                           # raw API response verbatim (see below)
  <Season>/<Format>/_summary.json     # which Pokémon have data + a derived
                                       # teammate-appearance count (see caveat below)
```

`<Format>` is `Doubles` or `Singles`. `<Season>` is whatever season name the API
echoes back for the `Current` alias the script requests — historically a real name
like `Season M-3`, though it has also come back as the literal `Current`.

Two consequences, both of which bit on the first refresh after the Rust parser was
written:

- **A refresh can create a new directory rather than updating the old one**, and
  old seasons are never deleted. `data/` can therefore hold several seasons at
  once, only one of which is live.
- **`index.json`'s `season` field is the only reliable pointer to the current
  one.** Never hardcode a season directory name; `meta::MetaDex::load` reads the
  index (falling back to the lexicographically greatest directory), which is what
  stops it silently serving stale data after a rollover.

### Per-Pokémon file shape

Each `<pokemon-slug>.json` is the site's `GET /api/battle/:format/:name` response,
stored as-is:

```json
{
  "pokemon": "Garchomp",
  "format": "Doubles",
  "season": "Season M-3",
  "source": "pokemon_champions_assets/battle_data/Doubles/Garchomp.csv",
  "columns": ["pokemon", "column_position", "category", "rank", "name", "percentage", "..."],
  "rows": [
    {"category": "move", "rank": 1, "name": "Dragon Claw", "percentage": "89.1%", "percentage_value": 89.1, "...": "..."},
    {"category": "held_item", "rank": 1, "name": "Life Orb", "percentage": "57.4%", "...": "..."},
    {"category": "teammate", "rank": 1, "name": "Whimsicott", "percentage": "", "...": "..."},
    {"category": "ability", "rank": 1, "name": "Rough Skin", "percentage": "97.6%", "...": "..."},
    {"category": "stat_alignment", "rank": 1, "name": "Jolly", "stat_up": "Speed", "stat_down": "Sp. Atk", "...": "..."},
    {"category": "stat_points", "name": "", "hp_points": 2, "attack_points": 32, "...": "speed_points, etc"}
  ]
}
```

`rows` mixes six `category` values in one array — filter by `category` to get
moves / held items / teammates / abilities / natures (`stat_alignment`) / EV spreads
(`stat_points`).

**`stat_points` are in the 0–32 stat-points authoring scale, summing to 66 — the
same units a teamsheet's `EVs:` line uses, *not* the 0–252 EV scale.** (Verified
across the whole cache: the observed maximum in any single field is exactly 32,
and the overwhelming majority of spreads total exactly 66.) Anything consuming
these must convert with `ev = max(0, 8p − 4)` — which
`state::pokemon::build_pokemon_state` already does for you when its
`use_stat_points` argument is set, so pass the raw points rather than
pre-scaling, or the formula gets applied twice.

### Percentages are raw, unnormalized site values

They are in 0–100 and do **not** sum to 1, nor reliably to 100:

| category | typical sum | why |
|---|---|---|
| `move` | ~350 | **marginal inclusion rates**, ≈4 slots × 100% — not a distribution over movesets |
| `held_item` / `stat_alignment` / `stat_points` | 68–100 | genuine distributions, top-N truncated; the remainder is unlisted "other" options |
| `ability` | ~100 | effectively complete |
| `teammate` | 0 | this category carries no percentages at all — `rank` is the only signal |

Sums can also *exceed* 100 (109.1 observed) from the site's own rounding, so
renormalize by the actual sum and never assert a total.

A handful of rows carry a named option with a null percentage, and categories are
individually optional (Ditto has no `move` rows at all), so nothing here should be
unwrapped.

### Naming-convention gap (handled in Rust)

This site's Pokémon names/slugs (e.g. `Hisuian Zoroark` / `hisuian-zoroark`) don't
always match the Showdown-style names this repo's enums use (e.g. `ZoroarkHisui`).
Moves, items, abilities and natures all resolve by normalization alone; species do
not. `poke_rust/src/meta/names.rs` holds the mapping table, and treats an
unresolvable species as a **hard error** rather than falling through to
`Species::from_str` — that function returns `Species::Unknown(_)` for anything it
doesn't recognize, and `build_pokemon_state` then hands an unknown species
`[100; 6]` base stats, producing a Pokémon that looks entirely plausible while
being numerically wrong everywhere.

Expect to maintain that table: the site is not internally consistent about form
naming (it writes `Rotom Wash`, `Rotom Heat` … but `Fan Rotom`) and has renamed
entries between seasons. A refresh that introduces an unrecognized species will
fail loudly on load, which is the intended behaviour.

### Usage rank

`column_position` is constant within each file and is a dense `1..=N` **usage rank,
unique per format** (Doubles rank 1 is Garchomp). It is ordinal only — there is
still no absolute usage percentage anywhere in the payload.

Where an actual magnitude is needed, `_summary.json`'s `teammateAppearanceCounts`
(how often each Pokémon shows up as *someone else's* listed teammate, aggregated
across every Pokémon scraped that format) is the closest available proxy — treat it
as an approximation, not ground truth. If the site ever adds a genuine ranking
endpoint, prefer that instead.
