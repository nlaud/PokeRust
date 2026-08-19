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

## Bot opponent

A bot session lets the server choose the P2 command.
The client sends no `p2` field for that session.
The server returns the drawn command as `p2Reveal`.

`SetupPanel.tsx` pairs the algorithm with the information mode.
A search reads the true position, or it draws worlds from a belief.
Only a belief search plays under a fog-of-war mode.
Only a search of the true position plays under Perfect Information.

The picker shows all six algorithms and disables every one that cannot play.
A mode change replaces a selection that the new mode does not permit.
The load path replaces a stored pair the same way.
`POST /api/battles` returns 422 for a pair that still arrives.

`P2RevealPanel.tsx` shows both states of that draw:

- During the search it shows the exact simulation count and a progress bar.
  It also shows a "Choose current move" button and a "Change my move" button.
- After the turn resolves it shows the drawn command and the draw source.

The profile, progress, strategy, and reveal use one expandable card.
Approximation notes appear only while the user points to or focuses the
`Approx.` control.

The client waits for the current analysis job until that job stops.
The shared simulation-turn budget stops the full job.
The progress bar compares the exact count with that budget.
`POST /api/battles/{id}/analysis` stops the search and keeps its current
complete strategy.
A job that ends with no answer blocks one submission and reports the reason.
The next submission plays the turn.

The panel marks a uniform draw with a warning.
`poke_rust/src/solver/README.md` explains when the server falls back to that
draw.

### Show the opponent strategy

The setup panel holds one checkbox under the P2 solver profile.
The checkbox sets `botP2.revealStrategy` on the create request.
The setting is off by default.

The server is the gate.
A session without the setting sends no P2 strategy row.
A hotseat battle holds no profile, so it never sends one.

A session with the setting reads two strategies:

1. `GET /api/battles/{id}/analysis` carries the strategy of the current
   position. `P2RevealPanel.tsx` reads it once each second.
2. `POST /api/battles/{id}/turn` carries the strategy that supplied the drawn
   command. The response also names the row of that command.

The rows sort by rate, highest first.
A row with a rate of zero does not appear.
The response includes every positive-rate row.

Only a checkpoint of the current position carries rows.
A stale answer names actions of an older position.

This setting shows you what P2 plans.
Keep it off for a fair battle.

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

`TrackerSolverPanel.tsx` shows the solver answer for the current position.
The profile, progress, and answer use one card below the input instructions.
It starts closed.
An open panel grows upward, and it scrolls only when it passes the window.
Approximation notes appear only while the user points to or focuses the
`Approx.` control.

Open the panel, select an algorithm, set the raw limits, and start the search.
The controls hold the same knobs as the simulator setup panel.

Both panels expose these limits:

- Simulation-turn budget.
- Main depth and replacement depth.
- Damage rolls and critical-hit branches.
- Particles for a belief search.

The simulation-turn budget holds a "Scale automatically" box.
A checked box sends no budget, and the server derives one.
A sampled search spends about one turn simulation for each turn of depth, and
one rollout reads one particle.
The derived budget multiplies both numbers, so a deeper search keeps its
rollout count instead of losing accuracy.
Clear the box to set the budget directly.

These damage settings apply only to solver searches.
A played simulator turn still uses 16 damage rolls and critical-hit branches.

These endpoints control the search:

```text
POST   /api/solve
GET    /api/solve/{jobId}/events
DELETE /api/solve/{jobId}
```

`solveStore.ts` registers one job, opens the event stream, and holds each
answer. `POST` returns a job ID, and the search starts when the store opens the
stream.

The tracker holds a belief, not a concrete battle state.
The server draws one world from the belief with the determinizer.
An `ismcts`, `mccfr`, or `pimc` profile reads the belief itself, and it renders
its rows against the drawn world.
A `pimc` profile solves each drawn world by itself and averages the strategies.
It sends one answer for each world that it finishes.
A `doubleOracle` profile is exact for its depth, and it reads the drawn world
alone.

An exact search runs one search for each depth through the configured depth.
Each depth sends a complete answer. A `doubleOracle` search also sends one
answer after each round. The numbers move while the search goes deeper.

A sampled search starts at the requested depth.
It runs until it uses the shared simulation-turn budget.

The store keeps the last complete answer while the next depth runs.
The panel shows that answer, the depth in progress, and the count of answers so
far.
It also shows the exact simulation count for the full job.

The panel compares the last two complete answers.
It shows the change in win odds between the two depths.
It also names each action that entered the strategy and each action that left
it.

### Team preview

Before the first `leads` line, no side has a Pokemon on the field.
The panel then searches the team preview instead of a battle.
The server draws worlds from the team-preview belief and solves the mean
payoff matrix of those worlds.

Each row is one bring-and-lead choice, not one battle command.
The row reads `Lead A + B · back C + D`.
Both players play one strategy for every drawn world.
A real opponent reads its own hidden stats, so the answer names that limit in
a note.

An `ismcts`, `mccfr`, or `pimc` profile draws up to eight worlds.
The answer names the count, because one world gives the whole answer one guess
of the opponent's hidden data.

The panel shows the win odds of both players.
The tracker user typed both rosters, so both complete strategies appear.
A belief search mixes the opponent's private builds.
The panel labels that list as a summary, not as one playable strategy.

Every row is text. The panel never writes a command to the input bar.

Every committed turn cancels the running job on the server.
The stream then sends `cancelled`, and the last answer stays on screen.

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
| `leads [p\|o] <species>... [back <species>...]` | Send one side into battle |
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

#### The back clause

The opening `leads` line can also name what you brought but did not lead:

```text
leads p tyranitar raichu back gengar snorlax o charizard aerodactyl
```

The clause removes the rest of your sheet from the belief.
The solver panel then reads the team that you really brought.

These rules apply to the clause:

- Only the `p` side accepts it. The bring of the opponent is hidden.
- Only the opening `leads` line accepts it.
- Each species must be on your roster, and no species can repeat.
- The count must equal the bench size of the format.

The clause is optional.
A format that brings your whole sheet has nothing to declare.
Without the clause the solver panel adds a warning, because the drawn world
gives you a bench that the real game does not hold.

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
