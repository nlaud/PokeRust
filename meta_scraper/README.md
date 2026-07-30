# Meta scraper

This tool caches competitive usage data from [Pokemon Champions Battle Data](https://championsbattledata.com/).

The cache contains these data:

- Pokémon usage rank.
- Common teammates.
- Moves.
- Held items.
- Abilities.
- Natures.
- Stat-point spreads.

`poke_rust/src/meta/` reads the cache.
`poke_rust/src/information/determinize.rs` uses the cache to create playable opponent teams.

## Attribution

Credit **Pokemon Champions Battle Data** when you share or use this data.
Include a link to https://championsbattledata.com/.

This fan project has no affiliation with Pokémon, Nintendo, Game Freak, or Creatures Inc.

## Update the cache

Run this command from the repository root:

```sh
python meta_scraper/update_meta.py
```

On Windows, use `py` if `python` is not on `PATH`:

```sh
py meta_scraper/update_meta.py
```

The tool uses only the Python 3 standard library.
The update takes a few minutes and shows its progress.

Use filters for a smaller update:

```sh
python meta_scraper/update_meta.py --format Doubles --pokemon garchomp --pokemon tyranitar
```

An update replaces the current cache data.
The cache does not keep an edit history.

## Cache layout

`meta_scraper/data/` is not tracked by Git.

```text
data/
  index.json
  <Season>/<Format>/<pokemon-slug>.json
  <Season>/<Format>/_summary.json
```

`<Format>` is `Doubles` or `Singles`.
The API supplies `<Season>`.

`index.json` contains the active season.
Always use this value.
Do not use a fixed season directory name.

An update can create a new season directory.
The tool does not remove old season directories.

`MetaDex::load` reads `index.json`.
If the index is missing, it uses the last directory in lexical order.

## Pokémon files

Each Pokémon file contains the unchanged API response:

```json
{
  "pokemon": "Garchomp",
  "format": "Doubles",
  "season": "Season M-3",
  "source": "pokemon_champions_assets/battle_data/Doubles/Garchomp.csv",
  "columns": ["pokemon", "column_position", "category", "rank", "name", "percentage"],
  "rows": [
    {"category": "move", "rank": 1, "name": "Dragon Claw", "percentage_value": 89.1},
    {"category": "held_item", "rank": 1, "name": "Life Orb", "percentage_value": 57.4},
    {"category": "teammate", "rank": 1, "name": "Whimsicott"},
    {"category": "ability", "rank": 1, "name": "Rough Skin", "percentage_value": 97.6},
    {"category": "stat_alignment", "rank": 1, "name": "Jolly"},
    {"category": "stat_points", "hp_points": 2, "attack_points": 32}
  ]
}
```

Filter `rows` by `category`.
The API can omit a category.
It can also provide a null percentage.
Code must handle both cases.

## Stat points

`stat_points` uses the Champions 0–32 authoring scale.
A normal spread uses 66 total points.
Teamsheet `EVs:` lines use the same scale.

Do not treat these values as 0–252 EVs.

Use this conversion:

```text
ev = max(0, 8p - 4)
```

`build_pokemon_state` applies this conversion when `use_stat_points` is true.
Pass raw stat points to that function.
Do not convert the values first.

## Percentages

The API provides values from 0 through 100.
The values are not one normalized distribution.

| Category | Typical total | Meaning |
|---|---:|---|
| `move` | About 350 | Marginal move rates for about four slots |
| `held_item` | 68–100 | A truncated distribution |
| `stat_alignment` | 68–100 | A truncated distribution |
| `stat_points` | 68–100 | A truncated distribution |
| `ability` | About 100 | An almost complete distribution |
| `teammate` | 0 | Rank is the only signal |

A total can exceed 100 because the source rounds each value.
One observed total was 109.1.

Normalize a distribution by its actual total.
Do not require a total of 100.

Move rates are marginal inclusion rates.
Do not normalize them as a distribution over complete move sets.

## Names

The site and this project use different species names.
For example, the site uses `Hisuian Zoroark`.
The project enum uses `ZoroarkHisui`.

`poke_rust/src/meta/names.rs` maps these names.
Moves, items, abilities, and natures resolve through normal name cleanup.

An unknown species is an error.
Do not pass it to `Species::from_str`.
That function returns `Species::Unknown(_)`.
An unknown species receives incorrect `[100; 6]` base stats.

Update the mapping when the site changes a form name.

## Usage rank

`column_position` is a dense usage rank for one format.
The rank starts at 1.
It does not contain an absolute usage rate.

`_summary.json` contains `teammateAppearanceCounts`.
This value counts appearances in other Pokémon teammate lists.
Use it only as an approximate popularity signal.
