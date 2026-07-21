# PokeRust Frontend

Minimalist web UI for the PokeRust battle simulator: a **Teams** page
(Showdown-format teamsheets in localStorage, the default route), a **Formats**
page (ruleset cards with a curated Pokémon Champions item pool for ban lists),
a **Simulate** page (hotseat battles against the Rust engine), a **Tracker**
page (follow a real battle by typing what happened instead of driving a
simulated opponent — see "Tracker mode" below), and a **Benchmark** page (right-aligned in the navbar; runs the full turn-resolution
and fog-of-war-inference speed sweep, streamed live over Server-Sent Events
from `GET /api/benchmark` — see `poke_rust::benchmarking`. This is the same
unbounded sweep the offline `cargo bench` binaries run, recorded in
`poke_rust/benches/RESULTS.md`, so it can take several minutes; the page
shows real backend-reported progress, not a fake timer).
React + Vite + TypeScript + Tailwind CSS v4.

## Running

Two processes, both from the repo root:

```sh
# 1. API server (release build matters — turn resolution is compute-heavy)
cd poke_rust
cargo run --release --bin server          # binds http://127.0.0.1:3001

# 2. Frontend dev server
cd frontend
npm install
npm run dev                               # http://localhost:5173, proxies /api → :3001
```

The server takes the same dex-path flags as the CLI (`--poke-dex`,
`--move-dex`, defaults point at `../pokemon_info/` so run it from
`poke_rust/`), plus `--port` (default 3001).

## Architecture

```
src/
  api/types.ts        1:1 TS mirrors of the server DTOs (source of truth:
                      poke_rust/src/bin/server/dto.rs — keep in sync by hand)
  api/client.ts       typed fetch wrappers
  store/battleStore.ts  hotseat command wizard (zustand): P1 picks per-slot
                      commands, then P2, then both ship in one POST /turn
  store/settingsStore.ts  theme (light/dark/custom), persisted
  lib/storage.ts      localStorage schemas: pokerust.teams.v1, pokerust.formats.v1
  lib/sprites.ts      Showdown name → PokeAPI slug + sprite URL cache
                      (sprites are fetched at runtime, never committed)
  lib/eventText.ts    EventNode tree → log lines (chronological walk with a
                      slot→species resolver, since events carry slots not names)
  pages/simulate/     SetupPanel, BattleScreen, Arena, ControlPanel,
                      PokemonHUD, FieldIndicators, BattleLogSidebar,
                      TeamInfoSidebar
  pages/tracker/      TrackerSetupPanel, TrackerScreen, TrackerLogSidebar —
                      reuses PokemonHUD/FieldIndicators/eventText.ts unchanged
  pages/benchmark/    BenchmarkChart, ProgressBar — hand-rolled inline-SVG bar
                      chart + determinate progress bar (no charting
                      dependency); used by pages/BenchmarkingPage.tsx
  store/trackerStore.ts  single-perspective session store for tracker mode —
                      no hotseat flip, no command wizard; `submitText` posts
                      raw tracker-syntax text to the server each turn
```

## Tracker mode

Follows a real battle you're playing or watching elsewhere: instead of a move
selector, you type what happened (`o1 switch garchomp`, `p1 thunderbolt o1
45%`, `endofturn`, …) into a plain textarea and the server translates it into
the same `InformationEvent`/fog-of-war machinery the Simulate page's inference
engine already runs on. There is no simulated opponent and no per-slot command
flow — `POST /api/tracker/{id}/events` is the only turn-advancing call, and it
expects one or more complete turns (each ending in an `endofturn` line) in a
single request.

Because there's no opponent to simulate, `BattleView.p1`/`p2` for a tracker
session are rendered straight from the fog-of-war belief on the Rust side (see
`poke_rust/src/bin/server/tracker.rs`'s module doc) — the client-side handling
is unchanged from battle mode: `TrackerScreen` reuses `PokemonHUD` and
`FieldIndicators` as-is, and `TrackerLogSidebar` reuses `lib/eventText.ts`'s
`renderLog` unchanged, since both are pure functions of `BattleView`/
`TurnLogEntry[]` with no assumption baked in about where that data came from.

This is a Phase-1 MVP: a plain multiline textarea, not the rich inline editor
(ghost-text completions, autocomplete, arrow-key event navigation) described
in the tracker-mode design doc — that's a planned follow-up on top of this
same pipeline. `tracker_parse.rs`'s module doc lists the current grammar's
scope and known simplifications (e.g. every targeted move needs an explicit
target slot; guaranteed effects cover a starter set of abilities/moves, not
the full dex yet).

## Notes

- **Hotseat model**: the server never holds half a turn. The frontend collects
  P1's full command set, flips to P2, then submits both together.
- **Doubles targeting**: legal targets come from the server's pre-expanded
  command options — the client has no targeting rules. A multi-target move
  parks in `pendingAttack` and the Arena highlights clickable target slots.
- **Damage rolls**: the server resolves turns with the engine's sample mode
  (`simulator::sample_turn`) — one weighted trajectory instead of the full
  outcome tree — so every format runs at full 16-roll granularity. The
  `probability` in the turn response is the joint probability of the sampled
  trajectory.
- **Battle restore**: the active battle id lives in sessionStorage; a page
  refresh re-fetches state + full event log from `GET /api/battles/{id}`.
  Server sessions are in-memory — restarting the server loses battles.
- **Sprites**: resolved through the PokeAPI `pokemon/{slug}` endpoint with an
  exception table for forme names, cached in localStorage. Unknown slugs fall
  back to the species endpoint's default variety, then progressively strip
  forme suffixes (Champions-only megas render the base species sprite); total
  failures show a gray Pokéball placeholder.
- **Item catalog**: `lib/items.ts` is a static list — exactly the Pokémon
  Champions held-item pool (general items, Mega Stones, berries) — not a
  PokeAPI fetch. Item sprite slugs still resolve against the PokeAPI sprites
  repo; Champions-only Mega Stones have no sprite and render label-only.
