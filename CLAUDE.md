# CLAUDE.md

> Keep `CLAUDE.md` and `AGENTS.md` synchronized: whenever either file is updated, apply the same update to the other.

## Documentation map

- `README.md` (repo root) — CLI/web-UI usage and benchmark commands.
- `frontend/README.md` — web UI: running the dev server, architecture, frontend-specific mechanics notes.
- `poke_rust/src/information/README.md` — design doc for the fog-of-war inference engine; read before touching that module.
- `meta_scraper/README.md` — Python tool that caches Pokémon Champions competitive usage stats.
- `poke_rust/benches/RESULTS.md` — recorded turn-resolution benchmark results and analysis.

## Project Status

The backend is a mature, heavily-tested probabilistic battle simulator: turn
resolution, damage calculation, and a full fog-of-war inference engine (see
below) have all been through multiple audit rounds. The project is now
transitioning focus from backend to **frontend development** — see the
"Frontend Development" section below for what exists and what's
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
                          PokemonState/BattleState for the damage calc to use.
                          NOT simulator-runnable — see determinize.rs
  determinize.rs            Collapses a belief into ONE complete, playable
                          BattleState, sampling hidden attributes from meta/
  cps.rs                    Conditional-Poisson fixed-size subset sampler
                          (move sets from marginals; bench selection)
  compositions.rs           Uniform sampling of bounded integer compositions
                          (the fallback EV spread generator)
  README.md                Full design doc for this module — read before touching it
meta/                     Competitive usage statistics (championsbattledata.com)
  names.rs                 Champions name -> enum resolution; the species
                          override table. Unresolvable species are a HARD ERROR
  schema.rs                Drift-tolerant serde types for the raw JSON
  dex.rs                   MetaDex: load, lookup, renormalization, priors
solver/                   Perfect-information Nash solver (simultaneous-move
                          game-tree search over concrete states)
  mod.rs                   Public API: SolveConfig/SolveResult, solve, solve_seeded
  search.rs                BI / BIab / DOab recursion, serialized alpha-beta with
                          star1, transposition table, turn cache
  matrix.rs                Zero-sum matrix-game equilibrium (dense tableau simplex)
  actions.rs               Phase-aware joint-action enumeration; SHARED with the
                          HTTP server, which delegates its legal-command dispatch here
  eval.rs                  LeafEvaluator fn pointer + the default heuristic
  chance.rs                ChanceMode: how much of a turn's outcome distribution
                          the search descends into
tests/
  simulator_tests.rs        Main battle-mechanics test suite (~33,700 lines)
  inference_tests.rs         Inference-engine regression tests, named
                          test_s<NN>_*/roundtrip_s<NN>_* after the soundness finding
  determinize_tests.rs       Determinizer soundness, runnability and fidelity
  solver_tests.rs            Solver search tests; the load-bearing one is
                          all_algorithms_agree (proves the pruning is sound)
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
`information/README.md`; don't duplicate that detail here. **Read it before
modifying this module**, in the same spirit as the Bulbapedia research rule
above. There is no separate audit log — soundness-bug history lives in inline
comments, regression tests (`test_s<NN>_*`/`roundtrip_s<NN>_*` in
`inference_tests.rs`), and git log.

### Meta-driven determinizer (`meta/` + `information/determinize.rs`)

A belief is *bounds*, but `simulate_turn` needs an actual team. The determinizer
collapses an `UnknownBattleState` into one concrete, playable `BattleState`,
sampling everything hidden from the competitive usage cache in `meta_scraper/`.

```rust
determinize_seeded(seed, belief, meta_dex, pokemon_dex, move_dex, cfg)
    -> Result<Determinized, DeterminizeError>
```

**Sample-only, deliberately.** Per Pokemon the cache admits ~750k builds and four
opponents cross-multiply past 10^23, so there is no enumerate mode. `probability`
on the result is the joint probability of the draw sequence — a lower bound on the
state's probability, comparable only between draws from the same belief (the same
contract `sample_turn` carries).

Things worth knowing before touching it:

- **`materialize.rs` is not an alternative.** Its output is a damage-oracle
  skeleton: empty benches (every switch illegal), 0-PP move slots (every move
  illegal), a flat `0.5*max_hp` HP sentinel, and `evs`/`ivs` hardcoded beside an
  unrelated `stats` override. Its approximations are load-bearing for Pass 3 —
  don't "fix" them. `determinize.rs` builds through `build_pokemon_state` instead.
- **Stat points vs EVs.** The meta gives 0–32 authoring points;
  `build_pokemon_state` applies `ev = max(0, 8p − 4)` itself when `use_stat_points`
  is set; `PokemonState.evs` and the belief's `min_evs`/`max_evs` are both already
  scaled. Four encodings of one quantity — variables are named `raw_points` vs
  scaled `evs` throughout, and passing the wrong one applies the formula twice.
  The budget is 66 *points*, and it does not translate to a fixed EV total: the
  −4 is charged once per nonzero stat, so a fully-spent spread is `528 − 4k` EVs
  over k invested stats, up to **516**. Hence `ev_total_cap` defaults to 516, not
  the familiar 510 (S68) — 510 both rejected every fully-spent spread in the
  cache and let Pass 5 tighten `max_evs` below a true 252. Check budgets in
  points wherever the authoring unit is available.
- **Move percentages are marginals, not a distribution.** They sum to ~350, not
  400; the shortfall is real mass on moves outside the top-10 list. `cps.rs` models
  it with explicit residual slots rather than normalizing it away — normalizing
  would inflate every rate ~3% and force any move above ~97%.
- **The observer's own side is never sampled**, only copied. For an opponent a
  `None` move slot means "unrevealed"; for your own Pokemon it means "this Pokemon
  has three moves".
- **Soundness is checked with the existing oracle.** `determinize` redraws until
  `subset_check::collect_true_state_subset_violations` is clean (or the budget
  runs out, then warns). `check_determinization` covers that oracle's three
  documented blind spots: EVs/IVs, HP, and revealed move slots.
- **An unresolvable species name is a hard error**, never a fallback:
  `Species::from_str` returns `Species::Unknown(_)`, which `build_pokemon_state`
  gives `[100; 6]` base stats — a plausible-looking, wholly wrong Pokemon. Expect
  to maintain `meta/names.rs`'s override table as the site renames formes.

Tests that assert cache *contents* will break on every scraper run; the suite
asserts invariants and reads its fidelity targets from the loaded dex instead.

### Perfect-information Nash solver (`solver/`)

Given a concrete `MatchState`, computes each player's optimal **mixed** strategy
over their legal joint commands plus the win odds:
`solve(state, pokemon_dex, move_dex, &SolveConfig) -> Result<SolveResult, SolveError>`
(and a `solve_seeded` twin, mirroring `determinize`/`determinize_seeded`).

A Pokemon turn is a *simultaneous-move stochastic game*: both players commit
without seeing the other, then the engine returns a distribution over successors.
Minimax models that wrongly — it leaks one player's commitment — and its
deterministic "safest move" output is exploitable. Each node's value is the Nash
equilibrium value of its payoff matrix, which is why the output is a probability
per action. Implements Algorithms 1–4 of Bošanský, Lisý, Lanctot, Čermák &
Winands, *Algorithms for computing strategies in two-player simultaneous move
games*, AIJ 237:1–40 (2016). The paper's chance-node weight `P*(s,r,c,s')` is
exactly the `f64` `simulate_turn` already attaches to each successor.

Things worth knowing before touching it:

- **A matrix cell may never be a bound.** Alpha-beta style pruning returns a
  bound; an LP over interval-valued cells computes nonsense and does so silently.
  Cells are always evaluated over the full `[0, 1]` window. Bounds are legal only
  inside `serial_ab` (an ordinary alternating-move search, where cutoffs and
  star1 are sound) and as an `(α, β)` window that *came from* serialized bounds —
  those provably contain the true value, so narrowing to them keeps values exact.
  That invariant is what lets the transposition table store bare numbers.
- **Utilities are win probabilities in `[0, 1]`**, P1-positive. Not cosmetic: it
  supplies the globally valid `L = 0`, `U = 1` that star1 pruning needs.
- **Mid-turn decision points don't consume a ply.** A faint leaves a replacement
  phase and a self-switch leaves a pivot pending; both are real simultaneous-move
  nodes, but charging them depth would make a depth-3 search that hit two faints
  really look one turn ahead.
- **Cost is `simulate_turn`, not the LP** — hundreds of microseconds against a
  few. Judge a configuration by `SolveStats::turns_simulated`; `ChanceMode` is
  the lever that matters. `SolverAlgorithm::SerializedBounds` trades LPs for
  extra turn resolutions, which is the wrong direction here — it exists so the
  benchmark can measure that rather than assume it.
- **`solver::actions` is shared with the HTTP server**, which delegates its
  legal-command dispatch to it. Replacement-phase joint legality needs
  `user::replacement_commands_are_valid`, not `validate_battle_command_combination`.
- **Determinism is per-process, not per-machine.** `coalesce_branches` sorts by
  probability but drains a `HashMap` first, so equal-probability successors come
  out in a run-varying order. `search::resolve` re-sorts with a state-hash
  tiebreak — unconditionally, not just for the reducing `ChanceMode`s, since even
  exact enumeration sums a cell in list order and float addition is not
  associative. That makes repeated solves inside one process identical
  (`repeated_solves_are_identical`). It does **not** make two processes agree:
  the engine coalesces at every intermediate expansion level too, so a
  successor's probability can land a few ulps apart across runs, moving work
  counts by ~1%. Values agree bit for bit or within one ulp. Backward induction
  drifts as much as the pruning algorithms, which is what places the cause in the
  transition function rather than in the search — fixing it would mean changing
  `coalesce_branches` for every caller.
- Anti-OOM is structural: the search is depth-first (live memory is
  `O(depth × branching)`, not `O(tree)`), successors are consumed so each
  subtree drops before the next expands, and `node_budget` degrades to static
  evaluation with a warning rather than panicking.

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
the soundness finding they cover (see git log for the finding's history).

Solver tests live in `tests/solver_tests.rs`, plus inline `mod tests` in each
`solver/` file. `all_algorithms_agree` is the one that matters: backward
induction, serialized bounds and double oracle explore wildly different
fractions of the tree, so any unsound cutoff surfaces as a disagreement in the
value. Keep solver test positions small — a search's cost is the product of the
action counts and the branching factor at every ply, and these run in debug.

## Frontend

A React web UI (`frontend/` at the repo root) backed by an Axum HTTP server
(`poke_rust/src/bin/server/`). Full run instructions and frontend architecture
live in `frontend/README.md` — this section covers what backend-side work needs
to know.

### Crate layout

`poke_rust` is a lib + two bins: `src/lib.rs` declares the module tree and the
global statics; `src/main.rs` is the CLI driver; `src/bin/server/` is the HTTP
server (`main`, `routes`, `session`, `dto`, `mapping`, plus `tracker`,
`tracker_parse`, `tracker_effects` for tracker mode — see below). The in-crate
test suite imports via `crate::` paths and is unaffected by the split.

### Server design rules

- **Hand-written DTOs** (`dto.rs`), not serde derives on engine types — the
  payload enums are code-generated and DTOs emit display-name strings. The
  frontend mirrors them by hand in `frontend/src/api/types.ts`; changing
  `dto.rs` means updating that file too.
- `mapping.rs::event_kind_dto` has one exhaustive match over `EventKind` — a
  new engine event variant is a compile error there (by design; add the DTO
  variant, then the TS type, then a phrasing in `frontend/src/lib/eventText.ts`).
- Inbound commands are validated by membership: the server re-enumerates legal
  commands and checks the reconstructed `BattleCommand` against them, then
  runs `validate_battle_command_combination`. 422 with a reason on failure.
- Turn resolution uses `simulator::sample_turn` (engine sample mode), observer
  = `Some(Player::P1)`.
- Sessions are in-memory (`HashMap` behind a mutex) — a server restart loses
  battles.

### Tracker mode

A second, simpler session kind for following a real battle by typing what
happened instead of driving a simulated opponent (`/tracker` in the frontend).
`tracker.rs` owns the session type (`TrackerSession`) and its four handlers;
`tracker_parse.rs` is the authoritative text→`InformationEvent` grammar (see
its module doc for the Phase-1 grammar scope and simplifications);
`tracker_effects.rs` synthesizes the guaranteed reactions a user should never
have to type (Intimidate's `-1 atk`, a self-boost move's `BoostChanged`, a
weather setter's `WeatherChanged`, …), applied as a post-process pass over the
parser's output.

**The belief IS the state — there is no concrete `MatchState` backing a
tracker session.** `apply_information` already knows how to fold events into a
fog-of-war belief with no ground truth behind it (that's exactly what a
battle-mode belief already is); `mapping::battle_view_from_belief` renders a
`BattleView` straight from it, reusing the same belief-only helpers
`bench_pokemon_view_from_belief` already used for bench mons. Tracker sessions
never touch `legal_commands`/`sample_turn_raw` — there is no move selector,
the user submits free text instead. `POST /api/tracker/{id}/events` requires
the submitted text to consist of complete, `endofturn`-terminated turns,
applied to a scratch clone first and only committed if every turn succeeds
(the same all-or-nothing discipline `session::resolve_turn` uses).

### Performance constraint (important)

Full enumeration (`simulate_turn`) grows the branch tree multiplicatively
across actions: a doubles turn with two spread moves + secondary effects at
16 damage rolls exceeds system memory (observed >15 GB). For interactive
play use `simulator::sample_turn` — engine sample mode
(`DamageConfig::sample`) keeps a single weighted branch at every expansion
chokepoint and returns one `(state, events, probability)` where the
probability is the joint probability of the sampled trajectory. The server
uses it for all turns, so the frontend always requests 16 damage rolls.
Still run the server with `--release`.

### The benchmark endpoint

`GET /api/benchmark` runs all three sweeps — turn resolution, inference, solver
(`benchmarking::run_turn_speed`/`run_inference`/`run_solver`) — over a single SSE
stream. Each sweep tags its own `progress` events and emits its own `result` when
it finishes, so the page fills in per chart rather than waiting on the whole run.
`failed` takes down one sweep, not the stream; only `done` ends it. Adding a
sweep means a `BenchmarkResultDto` variant, a call in `run_benchmark`, and the
matching hand-mirrored types in `frontend/src/api/types.ts`.

**The sweeps run sequentially, and that is deliberate.** Running them
concurrently finishes far sooner, but three CPU-bound sweeps sharing a machine
(on a hybrid CPU, across different core types) report contended times that no
longer reproduce `benches/RESULTS.md` — a benchmark whose numbers can't be
compared to the recorded ones isn't worth the wall-clock it saves. Per-sweep
streaming is what buys responsiveness instead. Don't "optimize" this back into
`tokio::join!`.

**A sweep cannot be cancelled once started.** `spawn_blocking` tasks have no
cancellation point, and closing the SSE stream does not stop them — a client
that reloads mid-run and starts another would leave the first three churning.
`AppState::benchmark_running` is an `AtomicBool` guard that makes a second
concurrent run fail fast instead; it is cleared on the same path that emits
`done`, so a panicking sweep cannot wedge the endpoint shut. This was found by
driving the page, not by reading the code: aborted runs left orphaned sweeps
saturating the CPU and every later measurement drifted.

### Testing the server

`helper_scripts/` has none for this; smoke-test with curl against
`http://127.0.0.1:3001/api` (create battle → preview turn → commands →
attack turn), or the flow in `frontend/README.md`. `frontend/e2e/` has a
Playwright suite, and Playwright is the way to eyeball a UI change — drive
`npm run dev` and screenshot rather than reasoning about the layout.
