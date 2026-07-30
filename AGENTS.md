# AGENTS.md

Keep this file and `CLAUDE.md` synchronized.

## Documentation

Always use the `ste-writing` skill when you write or edit documentation or comments.
Use strict mode unless the user requests another mode.

- `README.md` explains CLI and web use.
- `frontend/README.md` explains the frontend and tracker grammar.
- `poke_rust/src/information/README.md` explains fog-of-war inference.
- `meta_scraper/README.md` explains the usage-data cache.
- `poke_rust/benches/RESULTS.md` records benchmark results.

## Project state

PokeRust is a probabilistic Pokémon Champions battle simulator.
The backend has extensive mechanics, inference, determinizer, and solver tests.
Current development focuses on the frontend.

`TODO.md` tracks planned work.
It is not a mechanics reference.

## Mechanics research

Research every mechanic before you change it.
Use Bulbapedia as the primary source.
Read the newest-generation Effect, In battle, Notes, and Trivia sections.

Use these URL forms:

```text
https://bulbapedia.bulbagarden.net/wiki/<Name>_(move)
https://bulbapedia.bulbagarden.net/wiki/<Name>_(Ability)
https://bulbapedia.bulbagarden.net/wiki/<Name>_(item)
https://bulbapedia.bulbagarden.net/wiki/<Name>_(Pokémon)
```

Check interactions with other moves, abilities, items, and field effects.
Implement Pokémon Champions behavior.
Do not use an older-generation rule unless Champions uses it.

## Commands

Run Rust commands from `poke_rust/`:

```sh
cargo build
cargo test
cargo run -- --p1 ../teamsheets/<file> --p2 ../teamsheets/<file> -v 3
cargo test <test_name>
cargo clippy
```

## CLI options

| Option | Default | Purpose |
|---|---|---|
| `--p1`, `--p2` | Required | Teamsheet paths |
| `--poke-dex` | `../pokemon_info/showdownDex.txt` | Species data |
| `--move-dex` | `../pokemon_info/showdownMoves.txt` | Move data |
| `--ability-dex` | `../pokemon_info/showdownAbilities.txt` | Ability data |
| `--learnset-dex` | `../pokemon_info/showdownLearnsets.txt` | Learnset data |
| `--verbosity` | `1` | Debug level from 0 through 4 |
| `--no-consider-crit` | False | Disable critical-hit branches |
| `--damage-rolls` | `16` | Damage rolls from 1 through 16 |
| `--shared-multihit-damage-rolls` | False | Share one roll between hits |
| `--stat-points` | True | Use the stat-point formula |

## Architecture

The simulator returns weighted outcome branches.
Random damage, critical hits, accuracy, and other random effects create branches.

`coalesce_branches` merges equal final states and adds their probabilities.
It must not merge different event histories.

The engine clones state for each branch.
This is an intentional design.

### Source layout

```text
poke_rust/src/
  main.rs                 CLI
  user.rs                 Interactive driver and text output
  data/                   Generated enums
  state/                  Battle, Pokémon, and dex types
  simulator/              Turn resolution
  information/            Fog-of-war inference
  meta/                   Usage data
  solver/                 Nash solver
  bin/server/             Axum API
  tests/                  Rust tests
```

Do not edit files in `data/`.
Regenerate them with `helper_scripts/`.

### Core state

`MatchState` is one of these states:

- `TeamPreviewState`
- `BattleState`
- `GameOverState`

`BattleState` stores active Pokémon, bench Pokémon, actions, and field effects.

`PokemonState` stores identity, HP, moves, stats, status, item, ability, Tera, and Mega data.

Player input uses `PlayerCommand`.
A battle command can attack, switch, struggle, or pass.

### Simulator API

- `team_preview_state_from_teamsheets` creates a preview state.
- `get_possible_commands_for_active_slot` returns legal slot commands.
- `simulate_turn` returns all weighted outcomes.
- `sample_turn` returns one weighted outcome.
- `validate_battle_command_combination` checks joint legality.

Use `sample_turn` for interactive doubles.
Full enumeration can use more than 15 GB for one complex doubles turn.

## Fog-of-war inference

Read `poke_rust/src/information/README.md` before you change this module.

`apply_information` uses six passes:

1. Apply structural facts.
2. Infer item and ability presence or absence.
3. Convert damage to stat bounds.
4. Convert action order to Speed bounds.
5. Convert stat bounds to EV, IV, and nature bounds.
6. Propagate Boolean constraints.

Inference must remain sound.
It must not exclude a possible true value.

Regression tests use `test_s<NN>_*` and `roundtrip_s<NN>_*` names.

## Determinizer

`determinize_seeded` samples one playable state from a belief.
It uses the competitive usage cache.

Do not use `materialize.rs` as a determinizer.
Its output is only a damage-calculation state.

### Stat points

The usage cache stores authoring points from 0 through 32.
`build_pokemon_state` converts them to EVs.
Do not convert them twice.

Champions permits 66 points.
The largest scaled EV total is 516.
Keep `ev_total_cap` at 516.

### Move rates

Move percentages are marginal rates.
They total about 350, not 400.

`cps.rs` represents unlisted moves with residual slots.
Do not normalize move rates as a complete distribution.

### Other rules

- Copy the observer's side.
- Sample only hidden opponent data.
- Treat an unknown species name as an error.
- Redraw until the subset check succeeds or the retry limit expires.
- Keep returned probabilities comparable only within one belief.

## Solver

The solver computes a mixed Nash strategy for a concrete state.
It models each turn as a simultaneous stochastic game.

Do not replace this model with minimax.
Minimax reveals one player's action to the other player.

The solver implements backward induction, serialized bounds, and double oracle.
It solves each payoff matrix as a zero-sum game.

### Solver invariants

- Utilities are P1 win probabilities from 0 through 1.
- A matrix cell must contain an exact value.
- Alpha-beta bounds can exist only inside serialized search.
- Replacement and pivot nodes do not consume a turn depth.
- Search cost comes mainly from `simulate_turn`.
- Search must remain depth first.
- A node-budget failure must use static evaluation and return a warning.

`solver::actions` also supplies legal commands to the HTTP server.
Use `replacement_commands_are_valid` during replacement phases.

Sort equal-probability successors with a state-hash tie breaker.
This makes repeated solves stable within one process.
Different processes can still differ by a few floating-point units.

Keep solver test positions small.
Run `all_algorithms_agree` after a solver change.

## Server

The crate has a library, CLI binary, and server binary.
Server code is in `poke_rust/src/bin/server/`.

### DTO rules

Use hand-written DTOs.
Do not add Serde derives to engine state.

Update `frontend/src/api/types.ts` after a DTO change.

`mapping.rs::event_kind_dto` must match every `EventKind`.
Also update `frontend/src/lib/eventText.ts` after an event change.

### Command validation

The server rebuilds all legal commands.
It accepts a submitted command only when that command is in the legal set.
It then validates the full command combination.

Return HTTP 422 for an invalid command.

### Sessions

Sessions use an in-memory map behind a mutex.
A server restart removes all sessions.

## Tracker

Tracker mode records typed events from a real battle.
It does not have a concrete `MatchState`.
The belief is the tracker state.

`tracker_parse.rs` defines the text grammar.
`tracker_effects.rs` adds deterministic effects.
`tracker_render.rs` converts events back to tracker text.

The server applies complete turns to a scratch belief first.
It commits the result only when every turn succeeds.

Tracker sessions never call legal-command or turn-simulation functions.

## Benchmark endpoint

`GET /api/benchmark` streams turn, inference, and solver sweeps through SSE.
Run the sweeps in order.
Do not run them concurrently.

Concurrent sweeps produce contended times that do not match recorded results.

Each sweep sends its own progress and result events.
A failed sweep does not stop later sweeps.
Only `done` ends the stream.

`benchmark_running` prevents two active benchmark runs.
Closing the client stream does not cancel a `spawn_blocking` sweep.

## Tests

Main test files:

- `simulator_tests.rs`
- `inference_tests.rs`
- `determinize_tests.rs`
- `solver_tests.rs`

Test helpers are in `simuilator_test_helpers.rs`.

Useful helpers:

- `battle_state_from_lists`
- `run_single_turn`
- `damage_distribution`
- `assert_distribution_close`
- `hit_probability`

Use Playwright for frontend layout and interaction checks.
Start both development servers and capture screenshots for visual changes.
