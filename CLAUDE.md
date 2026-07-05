# CLAUDE.md

## Project Status

The backend is a mature, heavily-tested probabilistic battle simulator: turn
resolution, damage calculation, and a full fog-of-war inference engine (see
below) have all been through multiple audit rounds. The project is now
transitioning focus from backend to **frontend development** — see the
"Frontend Development" section below for what exists (nothing yet) and what's
planned.

## Game Version

This simulator targets the **newest generation of Pokémon — Pokémon Champions**.
All mechanics, species, moves, items, and abilities are implemented to match that
game. `TODO.md` at the repo root is no longer a mechanic-completion tracker — it
currently tracks frontend build-out (see "Frontend Development" below). If you
do pick up mechanic-implementation or mechanic-fix work, the Bulbapedia-research
rule below still applies in full.

## Research Requirements

Pokémon mechanics are notoriously full of edge cases, exceptions, and interaction
quirks that are not obvious from a move or ability's surface description. **Always
do web research before implementing or modifying any mechanic**, even ones that
seem straightforward. A move's Bulbapedia article routinely documents a dozen
edge cases that would otherwise be silently wrong in the simulator. ALWAYS RESEARCH ON BULBAPEDIA BEFORE SEARCHING CODE OR FORMING THE PLAN, TODO.md is not a source of truth.

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
| `--ability-dex` | `../pokemon_info/showdownAbilities.txt` | Ability data (used by the inference engine for ability absence/priority reasoning) |
| `--learnset-dex` | `../pokemon_info/showdownLearnsets.txt` | Learnset data (used by the inference engine for Illusion narrowing) |
| `-v` / `--verbosity` | `1` | Debug output level (0–4) |
| `--no-consider-crit` | false | Disable crit branching |
| `--damage-rolls` | `16` | Number of damage rolls to branch on (1–16) |
| `--shared-multihit-damage-rolls` | false | Use one shared damage roll across all hits of a multi-hit move |
| `--stat-points` | true | Use stat-points formula instead of EVs |

## Architecture

The project is a probabilistic Pokémon battle simulator. It loads two teams, then resolves battles turn-by-turn, producing weighted outcome trees instead of single deterministic results.

### Module layout (`poke_rust/src/`)

```
main.rs                   CLI entry point; loads dex data; drives simulation
user.rs                   Interactive battle driver (team preview -> turn loop ->
                          game over) and CLI text output formatting
data/                     Code-generated enums — do not hand-edit, regenerate
                          via helper_scripts/ instead
  species.rs              Species enum (~1,000 entries)
  pokemon_move.rs          PokemonMove enum (~2,900 entries)
  ability.rs               Ability enum (~300 entries)
  item.rs                  Item enum (~1,000 entries)
state/                    Core state types
  battle.rs                MatchState, BattleState, Action, FieldSlot, Player,
                          BattleCommand/PlayerCommand/TeamPreviewCommand
  pokemon.rs               PokemonState, teamsheet parsing
  dex_data.rs              Dex file parsing (species/move/ability/learnset),
                          shared enums (Status, Weather, Terrain, MoveFlag, …)
simulator/                Public API + turn resolution engine
  mod.rs                   team_preview_state_from_teamsheets, simulate_turn,
                          get_possible_commands_for_active_slot,
                          validate_battle_command_combination (~7,400 lines)
  helpers.rs               Internal mechanics: damage calc, ability/item hooks,
                          weather/terrain, end-of-turn resolution (~12,700 lines)
information/              Fog-of-war inference engine (partial-information
                          tracking for the opponent's hidden team data)
  information.rs           InformationEvent / EventKind: the vocabulary of what
                          a player can observe, as a nested action->reaction tree
  unknowns.rs              UnknownBattleState / UnknownPokemonState, the
                          Unknown<T> lattice, and the CNF Statement predicate store
  inference.rs             Six-pass engine; entry point apply_information
  inference/bcp.rs          Boolean constraint propagation pass
  materialize.rs            Turns a fog-of-war hypothesis back into a concrete
                          PokemonState/BattleState for the damage calc to use
  README.md                Full design doc for this module — read before touching it
  AUDIT.md                 Running soundness-bug log (S1–S29) with regression tests
tests/
  simulator_tests.rs        Main battle-mechanics test suite (~33,700 lines)
  inference_tests.rs         Inference-engine regression tests, named
                          test_s<NN>_*/roundtrip_s<NN>_* after the AUDIT.md finding
  simuilator_test_helpers.rs Test builders/helpers (battle builders, damage
                          helpers, assert functions)
helper_scripts/            Python scripts to regenerate data/ enums from Showdown source files
```

### Core design: probabilistic branching

Every public simulation function returns a weighted list of possible resulting states and their probabilities. Damage rolls, crits, and RNG effects each fork the outcome tree. `simulator::helpers::coalesce_branches()` merges structurally identical states, summing their probabilities — it must not merge branches with divergent event histories, only truly identical resulting states.

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

`BattleState` implements `Display` for human-readable CLI output (used by
`user.rs`) — this is text, not JSON. No state type currently derives
`Serialize`/`Deserialize` (see "Frontend Development" below).

Player-supplied actions use a two-level command model: a player submits one
`PlayerCommand` per turn (`Battle(Vec<BattleCommand>)`, `Pass`, or
`TeamPreview(TeamPreviewCommand)`), and each active slot's `BattleCommand` is
`Attack(AttackCommand)`, `Switch(SwitchCommand)`, `Struggle`, or `Pass`.

### Public API (`simulator/mod.rs`)

- `team_preview_state_from_teamsheets()` — parse team files into a `TeamPreviewState`
- `get_possible_commands_for_active_slot()` — enumerate legal `BattleCommand`s for a slot
- `simulate_turn(state, p1_cmd, p2_cmd, move_dex, pokemon_dex, consider_crit, damage_rolls, observer)` —
  core turn resolution; returns `Vec<(MatchState, Option<Vec<InformationEvent>>, f64)>`.
  The `InformationEvent` log (present when `observer` is `Some`) is the
  observer's-eye-view event stream for that branch, consumed by the inference
  engine below.
- `validate_battle_command_combination()` — validate a set of per-slot commands is jointly legal

`simulator/helpers.rs` exposes ~100 additional `pub fn`s (damage calculation,
type effectiveness, status/volatile handling, weather/terrain, ability hooks,
end-of-turn resolution, etc.) — these are internal engine mechanics, not a
driver-facing API.

### Fog-of-war inference engine (`information/`)

A Pokémon battle is played under partial information: each side sees moves,
switches, damage, and status changes on the field, but not the opponent's
EVs/IVs/nature/held item or (until revealed) ability or exact species form.
`information/` turns the stream of player-visible events from a battle into
the tightest possible **sound** bounds on everything hidden about the
opponent's team — it never excludes a value that could actually be true;
where multiple explanations remain consistent it keeps their union.

Entry point: `apply_information(state, events, dex, config)`, a six-pass
pipeline (speed-order → per-event structural facts → item/ability
presence-absence → damage→stat-bounds → EV/IV/nature back-solve → boolean
constraint propagation to a fixpoint). The full design — the
`InformationEvent`/`EventKind` event vocabulary, the `Unknown<T>` lattice, the
CNF `Statement` predicate store, and each pass in depth — is documented in
`information/README.md`; don't duplicate that detail here. `information/AUDIT.md`
logs every soundness bug found and fixed in this engine (S1–S29) with its
regression test — **read both before modifying this module**, in the same
spirit as the Bulbapedia research rule above.

### Global configuration

Verbosity and some roll-sharing flags are stored in `OnceLock<AtomicBool>` globals, set once at startup from CLI args. Functions throughout the codebase read these directly rather than threading config through arguments.

### Data enums

`data/species.rs`, `data/pokemon_move.rs`, `data/ability.rs`, and `data/item.rs` are **code-generated** from Showdown data files using `helper_scripts/gen_enums.py` and `helper_scripts/gen_items.py`. Do not edit them by hand — regenerate from source data instead.

## Testing

Tests live in `tests/simulator_tests.rs` (battle mechanics) and
`tests/inference_tests.rs` (fog-of-war engine), using helpers from
`tests/simuilator_test_helpers.rs`:

- `battle_state_from_lists()` — build a `BattleState` from Pokemon lists
- `run_single_turn()` — execute one turn and return outcome branches
- `damage_distribution()` — extract probabilistic damage outcomes
- `assert_distribution_close()` — fuzzy probability assertion
- `hit_probability()` — compute hit chance across branches

Inference regression tests are named `test_s<NN>_*` / `roundtrip_s<NN>_*` after
the `AUDIT.md` finding they cover.

## Frontend
Write this as you go.
