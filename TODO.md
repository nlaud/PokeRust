# TODO: Always remove items from here when they are completed :)

### Fixes
Tracker improvements:
- Defiant (and likely other stat-drop-triggered reactive-boost abilities, e.g. Competitive) has no guaranteed-reaction synthesis in `tracker_effects.rs`'s ability table — a Defiant mon's own +2 Atk after being Intimidated never gets synthesized, causing a tracker round-trip mismatch. Found via the tracker subset-oracle fuzzer with real teams (first repro: seed 100004+, `POKERUST_TRACKER_FUZZ_REAL_TEAMS=1`).

### New features

- Test the meta sampler & make sure a chunk of the data looks good...
  - Now on the Rust end, create a determinizer that takes in the meta state (we will need a parser for this) + an unknownbattlestate, then, like the simulator, has modes for giving a single random sample state or giving every possible state with probability (This should not consider "other" options, just the normal ones, it should force based on the meta percent and known information items, moves, etc.). It should output complete full states though that should just be able to be put in the simulator and work!
- Eventually create nash solver and recursive evaluation (When both players have perfect information)
  - [ ] Then move on to actual bot creation, battle and mentor pages (Could make these an option in the simulate / tracker page?).
