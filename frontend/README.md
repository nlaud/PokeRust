# PokeRust Frontend

Minimalist web UI for the PokeRust battle simulator: a **Simulate** page
(hotseat battles against the Rust engine), a **Teams** page (Showdown-format
teamsheets in localStorage), and a **Formats** page (ruleset cards, also
localStorage). React + Vite + TypeScript + Tailwind CSS v4.

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
```

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
  exception table for forme names, cached in localStorage. Failures fall back
  to a gray Pokéball placeholder.
