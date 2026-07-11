# meta_scraper

Local cache of Pokémon Champions competitive usage stats — common Pokémon, common
partners (teammates), items, moves, abilities, and EV/nature spreads — scraped from
**[Pokemon Champions Battle Data](https://championsbattledata.com/)**, sourced via
that site's own documented, no-auth JSON API rather than screen-scraping.

This is the data-collection half of `TODO.md`'s longstanding "meta sampler" feature
(originally scoped against pikalytics, but championsbattledata.com is the actual
usage-stats site for this game). The eventual consumers are the simulator's planned
meta-aware bot/Nash solver and a frontend "Tracker page" (see `TODO.md`).

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

`<Format>` is `Doubles` or `Singles`. `<Season>` is the *resolved* season name (e.g.
`Season M-3`) — the update script always requests the site's `Current` alias, but
stores using the real name the API echoes back, so the on-disk layout stays
meaningful even after the site rolls over to a new season.

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
(`stat_points`, points already in the 0–252 scale, not the 0–32 stat-points authoring
units the teamsheets use).

### Known naming-convention gap

This site's Pokémon names/slugs (e.g. `Hisuian Zoroark` / `hisuian-zoroark`) don't
always match the Showdown-style names this repo's teamsheets use (e.g.
`Zoroark-Hisui`). There's no cross-referencing table here yet — a future consumer
that needs to join this data against `pokemon_info/` or a teamsheet will need to
handle that mapping itself.

### The "common Pokémon" approximation

The API has **no site-wide usage-rank field for a Pokémon itself** — its endpoints
only expose per-Pokémon breakdowns (moves/items/teammates/abilities/spreads used
*on* that Pokémon), not "how often is this Pokémon used at all" across the whole
metagame. `_summary.json`'s `teammateAppearanceCounts` (how often each Pokémon shows
up as *someone else's* listed teammate, aggregated across every Pokémon scraped that
format) is the closest available proxy for "how central is this Pokémon to the
metagame" — treat it as an approximation, not ground truth. If the site ever adds a
genuine ranking endpoint, prefer that instead.
