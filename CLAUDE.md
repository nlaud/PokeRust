# CLAUDE.md

## Game Version

This simulator targets the **newest generation of Pokémon — Pokémon Champions**.
All mechanics, species, moves, items, and abilities are implemented to match that
game. The `TODO.md` at the repo root tracks the Pokémon Champions moves, items,
abilities, and berries that still need work; an entry is removed from `TODO.md`
once its full effect is implemented in the simulator.

## Research Requirements

Pokémon mechanics are notoriously full of edge cases, exceptions, and interaction
quirks that are not obvious from a move or ability's surface description. **Always
do web research before implementing or modifying any mechanic**, even ones that
seem straightforward. A move's Bulbapedia article routinely documents a dozen
edge cases that would otherwise be silently wrong in the simulator. ALWAYS RESEARCH ON BULBAPEDIA BEFORE FINALIZING YOUR PLANS, TODO.md is not a source of truth.

### Bulbapedia

Bulbapedia is the primary reference. Article URLs follow a predictable pattern:

```
https://bulbapedia.bulbagarden.net/wiki/<Name>_(move)
https://bulbapedia.bulbagarden.net/wiki/<Name>_(ability)
https://bulbapedia.bulbagarden.net/wiki/<Name>_(item)
https://bulbapedia.bulbagarden.net/wiki/<Pokémon_name>_(Pokémon)
```

Spaces in names become underscores. Disambiguation suffixes (`_(move)`,
`_(Ability)`, `_(item)`) are added when the name is shared with a Pokémon or
other article. When in doubt, try with and without the suffix.

Each article's **Effect** and **Description** sections describe the base
behaviour. The **In battle** subsection and any **Trivia / Notes** at the bottom
are where generation-specific exceptions and interaction quirks live — always
read those before coding. Always implement the **newest-generation behaviour**
(Pokémon Champions), not behaviour from older games.

### What to look for

When researching a mechanic, specifically check for:

- **Interactions with other moves/abilities/items** (e.g. does this move still
  work under Magic Room? under Mold Breaker?)

## Commands

All commands run from `poke_rust/`:

```sh
cargo build
cargo test
cargo run -- --p1 ../teamsheets/<file> --p2 ../teamsheets/<file> -v 3
```

Run a single test by name:
```sh
cargo test <test_name>
```

Lint:
```sh
cargo clippy
```

## CLI Flags

| Flag | Default | Description |
|---|---|---|
| `--p1`, `--p2` | required | Teamsheet file paths |
| `--poke-dex` | `../pokemon_info/showdownDex.txt` | Pokemon species data |
| `--move-dex` | `../pokemon_info/showdownMoves.txt` | Move definitions |
| `-v` / `--verbosity` | `1` | Debug output level (0–4) |
| `--no-consider-crit` | false | Disable crit branching |
| `--damage-rolls` | `16` | Number of damage rolls to branch on (1–16) |
| `--stat-points` | true | Use stat-points formula instead of EVs |

## Architecture

The project is a probabilistic Pokémon battle simulator. It loads two teams, then resolves battles turn-by-turn, producing weighted outcome trees instead of single deterministic results.

### Module layout (`poke_rust/src/`)

```
main.rs                   CLI entry point; loads dex data; drives simulation
simulator.rs              Public API — all external callers use only this module
simulator_helpers.rs      Internal battle mechanics (damage calc, stat boosts, ability/item effects)
battle.rs                 State types: MatchState, BattleState, PokemonState, Action
pokemon.rs                PokemonState construction & teamsheet parsing
dex_data.rs               Shared enums and parsing helpers (Status, Weather, Terrain, MoveFlag, …)
simulator_tests.rs        All tests (~5,600 lines)
simuilator_test_helpers.rs  Test utilities (battle builders, damage helpers, assert functions)
data/
  species.rs              Species enum (~1,000 entries, code-generated)
  moves.rs                PokemonMove enum (~2,900 entries, code-generated)
  abilities.rs            Ability enum (~300 entries, code-generated)
  items.rs                Item enum (~1,000 entries, code-generated)
helper_scripts/           Python scripts to regenerate data/ enums from Showdown source files
```

### Core design: probabilistic branching

Every public simulation function returns `Vec<(MatchState, f64)>` — a list of possible resulting states and their probabilities. Damage rolls, crits, and RNG effects each fork the outcome tree. `simulator_helpers::coalesce_branches()` merges structurally identical states, summing their probabilities.

State is cloned per branch — `BattleState` and `PokemonState` are `Clone`-heavy by design.

### Key types

```
MatchState
  ├── TeamPreviewState   — before leads are selected
  ├── BattleState        — active battle
  └── GameOverState      — terminal

BattleState
  ├── p1/p2_active_mons: Vec<PokemonState>
  ├── p1/p2_back_mons:   Vec<PokemonState>
  ├── action_queue:      Vec<Action>
  ├── Field effects:     weather, terrain, pseudo-weather
  └── Side/slot conditions, tera/mega tracking

PokemonState
  ├── species, types, level, hp, moves, move_pp
  ├── stats: PokemonStatsTable  — [hp, atk, def, spa, spd, spe]
  ├── boosts: PokemonBoostTable — [atk, def, spa, spd, spe, acc, eva]
  ├── status: Option<Status>
  ├── volatiles: Vec<VolatileStatusState>
  └── item, ability, tera_type, mega_species

Action
  ├── MoveAction
  ├── SwitchAction
  ├── MegaAction
  └── TeraAction
```

### Public API (simulator.rs)

- `team_preview_state_from_teamsheets()` — parse team files into initial `MatchState`
- `get_possible_commands_for_active_slot()` — enumerate legal actions for a slot
- `simulate_turn()` — core turn resolution; returns `Vec<(MatchState, f64)>`
- `validate_battle_command_combination()` — validate a chosen action pair

### Global configuration

Verbosity and some roll-sharing flags are stored in `OnceLock<AtomicBool>` globals, set once at startup from CLI args. Functions throughout the codebase read these directly rather than threading config through arguments.

### Data enums

`data/species.rs`, `data/moves.rs`, `data/abilities.rs`, and `data/items.rs` are **code-generated** from Showdown data files using `helper_scripts/gen_enums.py` and `helper_scripts/gen_items.py`. Do not edit them by hand — regenerate from source data instead.

## Testing

Tests live in `simulator_tests.rs` and use helpers from `simuilator_test_helpers.rs`. Key helpers:

- `battle_state_from_lists()` — build a `BattleState` from Pokemon lists
- `run_single_turn()` — execute one turn and return outcome branches
- `damage_distribution()` — extract probabilistic damage outcomes
- `assert_distribution_close()` — fuzzy probability assertion
- `hit_probability()` — compute hit chance across branches
