# PokeRust frontend

The frontend uses React, Vite, TypeScript, and Tailwind CSS.

It provides these pages:

- **Teams** stores Showdown teamsheets.
- **Formats** stores rules and item bans.
- **Simulate** runs a hotseat battle.
- **Tracker** records events from a real battle.
- **Benchmark** runs and displays server benchmarks.

## Start the application

Start the API server from the repository root:

```sh
cd poke_rust
cargo run --release --bin server
```

The server listens on http://127.0.0.1:3001.
Use a release build because turn resolution uses much CPU time.

Start the frontend in another terminal:

```sh
cd frontend
npm install
npm run dev
```

Open http://localhost:5173.
The development server sends `/api` requests to port 3001.

The API server accepts the CLI dex-file options.
It also accepts `--port`.
Run the server from `poke_rust/` when you use the default dex paths.

## Test the frontend

Build the release server once:

```sh
cd poke_rust
cargo build --release --bin server
```

Install Chromium once:

```sh
cd frontend
npx playwright install chromium
```

Run the end-to-end tests:

```sh
npx playwright test
```

Playwright starts the API and frontend servers.
Outside CI, it uses compatible servers that already run.

Run the static checks:

```sh
npx tsc -b
npm run lint
```

The project does not have a frontend unit-test runner.

## Source layout

```text
src/
  api/
    client.ts              Typed API requests
    types.ts               Manual copies of Rust DTOs
  lib/
    eventText.ts           Event tree to battle-log text
    sprites.ts             Sprite lookup and cache
    storage.ts             Browser-storage schemas
    trackerGrammar.ts      Tracker completion rules
  store/
    battleStore.ts         Hotseat command flow
    benchmarkStore.ts      Benchmark stream state
    settingsStore.ts       Saved display settings
    trackerStore.ts        Tracker history, preview, and solver state
  pages/
    benchmark/             Benchmark charts
    simulate/              Battle controls and display
    tracker/               Tracker controls and display
e2e/                       Playwright tests
```

`poke_rust/src/bin/server/dto.rs` is the DTO source.
Update `src/api/types.ts` after a DTO change.

`tracker_parse.rs` is the tracker grammar source.
`trackerGrammar.ts` supplies completion only.
The server always validates tracker input.

## Benchmark page

`GET /api/benchmark` runs three sweeps:

1. Turn resolution.
2. Fog-of-war inference.
3. Game-tree solving.

The server runs the sweeps in order.
Concurrent sweeps would compete for CPU time and make results hard to compare.

One SSE stream carries all results.
Each event includes its sweep name.

The stream uses these event types:

- `progress` updates one sweep.
- `result` completes one sweep.
- `failed` fails one sweep.
- `done` closes the stream.

`benchmarkStore` keeps separate state for each sweep.
Each chart can finish while another chart continues.

The page keeps each card at a fixed size.
This prevents layout changes during a run.

`pages/benchmark/glossary.ts` defines benchmark terms.
Update the glossary after a related engine change.

## Tracker mode

Tracker mode records a battle that occurs outside the simulator.
The user types each observed event.
The server converts the text to `InformationEvent` values.
The inference engine then updates the belief.

Tracker mode has no simulated opponent.
It has no move selector.

Submit complete turns to:

```text
POST /api/tracker/{id}/events
```

Each turn must end with `endofturn` or `eot`.

The server renders both sides from the belief.
The tracker reuses the normal battle display and log renderer.

### Input editor

`TrackerInputBar.tsx` provides one-line completion.
It suggests tracker keywords, species, moves, abilities, and items.

The server supplies match-specific species, move, and ability choices.
The frontend supplies item choices from `lib/items.ts`.

Press `Enter` to preview one event.
The server applies structural facts to a temporary belief.
The preview does not change the session.

Press `Shift+Enter` to complete the turn.
The frontend sends the full authored history.
The server rebuilds the belief from the initial state.

Use `ArrowUp` to edit an earlier event.
The frontend resubmits the full history after the edit.

Press `Shift+Escape` to remove the last complete turn.
If a draft exists, the first press clears the draft.

### Solver panel

`TrackerSolverPanel.tsx` shows the solver answer for the last committed turn.
The panel appears below the input instructions.
It starts closed.

Open the panel, select an algorithm and a preset, then start the search.
The controls hold the same knobs as the simulator setup panel.

These endpoints control the search:

```text
POST   /api/tracker/{id}/analysis
GET    /api/tracker/{id}/analysis
DELETE /api/tracker/{id}/analysis
```

The tracker holds a belief, not a concrete battle state.
The server draws one world from the belief with the determinizer.
An exact search and a sampled search read that one world.
A belief search reads the belief and renders its rows against the drawn world.
Each answer names this limit in a note.

The server runs one search for each depth from one through the configured depth.
Each depth publishes a complete answer.
The store reads the newest answer one time each second.
The numbers therefore move while the search goes deeper.

The panel shows the win odds of both players.
It also shows the change since the last committed turn.
The tracker user typed both rosters, so both action lists appear.
A belief search mixes the opponent's private builds.
The panel labels that list as a summary, not as one playable strategy.

Every committed turn cancels the running search and starts a new one.
The panel marks an answer of an older position as stale.

## Tracker grammar

Input is line based.
Blank lines are valid.
A line that starts with `#` is a comment.

Names and keywords ignore case and punctuation.
Whitespace separates tokens.
Join the words in a multiword name.

For example, use `rockyhelmet` instead of `rocky helmet`.

```text
submission  := turn+
turn        := event-line* ("endofturn" | "eot")
slot        := ("p" | "o") [positive-integer]
hpspec      := <unsigned-integer>("%" | "hp")
boostspec   := <stat><signed-integer> | <signed-integer><stat>
```

`p` identifies the tracker owner.
`o` identifies the opponent.
Slot numbers start at 1.
An omitted number means slot 1.

During a normal turn, each occupied slot needs one action.
An action can be a move, switch, failure reason, recharge, or pass.

A knocked-out slot does not need an action.
A replacement turn needs switches only for slots that have a healthy reserve.

### Event lines

| Form | Result |
|---|---|
| `leads [p\|o] <species>...` | Send one side into battle |
| `[slot] switch <species> [hpspec]` | Switch one slot |
| `[slot] mega [species-or-suffix]` | Mega Evolve |
| `[slot] tera <type>` | Terastallize |
| `[slot] <move> [target-or-effect]...` | Record a move |
| `[slot] <ability>` | Reveal an ability |
| `[slot] <item>` | Reveal a held item |
| `[slot] <item-verb> <item>` | Change a held item |
| `[slot] hp <hpspec>` | Record HP outside a move |
| `[slot] <cant-reason>` | Record a failed action |
| `[slot] mustrecharge` | Add a recharge state |
| `[slot] charging <move>` | Record a charge turn |
| `[slot] pass` | Record no action |
| `weather <weather>` | Change or clear weather |
| `terrain <terrain>` | Change or clear terrain |

One `leads` line can include both sides:

```text
leads p tyranitar raichu o charizard aerodactyl
```

Place entry-ability lines directly after the leads line.
The parser attaches those abilities to the switch event.

### Move lines

A move line starts with the user and move.
Name each target.
A charge turn can omit its target.

```text
p1 thunderbolt o1 45% par
p1 rockslide o1 62% o2 miss
o1 tackle p1 88hp p1 helmet o1 91%
```

A slot token selects the current effect target.
Later effects apply to that slot.
Another slot token changes the effect target.

Repeat a slot to record multiple hits.

| Token | Result |
|---|---|
| `crit` | Critical hit |
| `miss`, `missed` | Miss |
| `immune` | Immunity |
| `block`, `blocked` | Protection |
| `fail`, `failed` | Complete move failure |
| `charging` | Charge turn |
| `45%`, `97hp` | New HP |
| `atk+1`, `-2spe` | Stat-stage change |
| Status word | Major status |
| Volatile word | Volatile status |
| Ability name | Ability reveal |
| Item name | Item reveal |
| Item verb and item | Item change |

The parser compares an HP value with the current belief.
A lower value means damage.
A higher value means healing.
An equal value means a direct HP set.

Use exact `hp` values for the owner.
Use `%` values for the opponent.

Submit turns separately when a later HP direction depends on an earlier turn.

`miss`, `immune`, and `blocked` stop guaranteed effects for one target.
`fail` stops guaranteed effects for the full move.
Type a random secondary effect only when it occurs.

### Charge turns

Add `charging` to the first turn of a two-turn move:

```text
o1 solarbeam charging
p1 protect
endofturn

o1 solarbeam p2 45% crit
p2 substitute
endofturn
```

The charge turn can omit its target.
The release turn uses a normal move line.

`charging` stops release effects during the charge turn.
The server still adds a guaranteed charge-turn boost.

Meteor Beam and Electro Shot raise Special Attack.
Skull Bash raises Defense.
Geomancy applies its boosts on release.

Do not use `charging` when Power Herb or weather skips the charge turn.

### Accepted words

Stats:

- `atk`, `attack`
- `def`, `defense`, `defence`
- `spa`, `spatk`, `spattack`, `specialattack`
- `spd`, `spdef`, `spdefense`, `specialdefense`
- `spe`, `speed`
- `acc`, `accuracy`
- `eva`, `evasion`, `evasiveness`

Major statuses:

- Burn: `brn`, `burn`, `burned`
- Poison: `psn`, `poison`, `poisoned`
- Toxic poison: `tox`, `badpoison`, `badlypoisoned`, `toxic`
- Paralysis: `par`, `para`, `paralyzed`, `paralysis`, `paralysed`
- Sleep: `slp`, `sleep`, `asleep`
- Freeze: `frz`, `frozen`, `freeze`

Weather:

- Rain: `rain`, `raindance`, `drizzle`
- Heavy rain: `heavyrain`, `primordialsea`
- Sandstorm: `sand`, `sandstorm`
- Snow: `snow`, `hail`
- Sun: `sun`, `sunnyday`, `sunny`, `drought`
- Extreme sun: `extremesun`, `desolateland`, `harshsunlight`
- Strong winds: `strongwinds`, `deltastream`
- Clear weather: `none`, `clear`

Terrain accepts its short or full name.
For example, use `electric` or `electricterrain`.
Use `none` or `clear` to remove terrain.

Item verbs:

- Lost: `loses`, `lost`, `knockedoff`
- Consumed: `consumes`, `consumed`, `ate`, `eats`, `used`
- Gained: `gains`, `gained`, `tricked`, `switcheroo`, `recycles`

Common item aliases:

- `sitrus`, `lum`, `chesto`
- `lefties`, `levs`, `helmet`, `lo`
- `scarf`, `specs`, `band`
- `boots`, `wp`, `av`, `sash`

The server also accepts normalized item names.

For failure reasons and volatile aliases, read the tables in `tracker_parse.rs`.
That file is the authoritative list.

### Automatic effects

The server adds deterministic results that follow from a typed observation.

It adds supported entry-ability effects.
Examples include Intimidate, weather setters, terrain setters, Intrepid Sword, and Dauntless Shield.

Mega Evolution reveals a fixed Mega ability.
The server then adds that ability's deterministic entry effect.

The server adds structured move effects with a 100% chance.
These can include boosts, status, weather, terrain, and side conditions.

The server never guesses a random secondary effect.
The user must type that effect.

An HP value of zero adds a faint event.

Type recoil, drain, reactive items, and reactive abilities when you observe them.

### Limits

The grammar does not represent every engine event.
It rejects unknown input.

Current limits include these cases:

- Payloads for some volatile statuses.
- Wish, Future Sight, and Doom Desire payloads.
- Form changes other than Mega Evolution, Tera, Illusion, and Transform.

Run the tracker fuzz tests:

```sh
cd poke_rust
cargo test --bin server randomized_tracker_text_round_trips_do_not_contradict
```

Set `POKERUST_TRACKER_FUZZ_ITERS` to increase the test count.
Set `POKERUST_TRACKER_FUZZ_SEED_START` to replay a seed range.
Set `POKERUST_TRACKER_FUZZ_REPLAY=1` to show a replay.

Run the slower soundness test:

```sh
cargo test --release --bin server -- --ignored randomized_tracker_text_beliefs_stay_sound_subset
```

## Implementation notes

The hotseat client collects all P1 commands before it collects P2 commands.
It sends both command sets in one request.

The server supplies all legal targets.
The client does not implement target rules.

The server uses `simulator::sample_turn`.
It returns one weighted trajectory with 16 damage rolls.

The active battle identifier uses session storage.
A page reload requests the state and event log again.
A server restart removes all in-memory sessions.

The frontend gets sprites from PokeAPI and caches their URLs.
If all lookups fail, it shows a gray Poké Ball.

`lib/items.ts` contains the Champions held-item list.
The frontend does not get this list from PokeAPI.
