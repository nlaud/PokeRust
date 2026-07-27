# TODO: Always remove items from here when they are completed :)

### Fixes
Tracker improvements:
- Defiant (and likely other stat-drop-triggered reactive-boost abilities, e.g. Competitive) has no guaranteed-reaction synthesis in `tracker_effects.rs`'s ability table — a Defiant mon's own +2 Atk after being Intimidated never gets synthesized, causing a tracker round-trip mismatch. Found via the tracker subset-oracle fuzzer with real teams (first repro: seed 100004+, `POKERUST_TRACKER_FUZZ_REAL_TEAMS=1`).
- `randomized_tracker_text_round_trips_do_not_contradict` loses a `SideConditionEnd { side: P2, condition: Reflect }` under `POKERUST_TRACKER_FUZZ_REAL_TEAMS=1` (the turn where a Heat Wave KOs into a Reflect expiring the same turn). Confirmed pre-existing at ff0978c, so it is not a charging/side-condition-masking regression. Likely `synthesize_expiry_clears` not firing when the same turn already ended the battle for that slot.
- e2e `tracker-input.spec.ts:15` ("autocomplete, leads, two-tier commit, delete-on-empty, and navigation") fails at its last assertion: after `Shift+Escape` the line-number gutter should reset to `1` but reads `3`, i.e. the whole-draft discard isn't clearing both saved draft lines. Confirmed pre-existing at ff0978c (verified by stashing all local work and re-running). The other 10 e2e tests pass.
- Meteor Beam / Electro Shot (+1 SpA) and Skull Bash (+1 Def) express their charge-turn boost in `onTryMove` JS that `parse_move_entry` doesn't read, so the engine hardcodes it (`simulator/mod.rs`) and `tracker_effects.rs` can only synthesize it when `charging` is typed. A Power Herb one-turn use therefore needs the boost typed by hand (`p1 meteorbeam o1 spa+1`) — it's indistinguishable from a release turn otherwise. Fixable by teaching the dex parser those blocks, or by tracking a `Charging` volatile on the belief for the pure-charge family the way `SemiInvulnerable` now is.
- `frontend/src/lib/trackerGrammar.ts` has drifted from `tracker_parse.rs`: `SLOT_VERB_WORDS`/`MOVE_EFFECT_WORDS` are missing `illusion`, `damage`/`heal`/`sethp`, `status`, `cure`, `volatileend`, `encoremove`, `disablemove`, `stockpilelevel`, `copyboosts`, `invertboosts`, `Nhits`; `FIELD_LINE_WORDS` is missing the `side` and `field`/`pseudoweather` line-start keywords; `VOLATILE_WORDS` stops at `forestscurse` (Rust also takes `throatchop`, `mustrecharge`/`recharging`, `substitute`/`sub`, `encore`, `disable`); and nothing knows about `@slot`. Completion-only, so nothing is unparseable — just undiscoverable.

### New features
Determinizer follow-ups — the core landed: `poke_rust/src/meta/` parses the usage
cache and `information/determinize.rs` samples a complete, playable `BattleState`
from a belief. What is left is all fidelity, not correctness; every item below
produces worlds that are legal but less plausible than they could be.

- `nature_spread_coherence` ships at `1.0` (off), so nature and spread are drawn
  independently and incoherent builds (Bold with 32 Atk points) do occur. The data
  carries the signal to fix it — `stat_up`/`stat_down` on every nature row — so try
  `0.15` and confirm `sampled_builds_follow_the_usage_data` still passes.
- `pre_transform` / `illusion_disguise` / `rest_sleep` are dropped when building a
  concrete Pokémon, and the belief's `possible_illusion_state` is never consumed —
  so a determinized world can't represent an active Transform or Illusion.
- Bench invention is the biggest source of implausible-but-legal worlds and is
  invisible to the subset oracle (an invented Pokémon contradicts nothing). It is
  warned on every occurrence; a better prior than teammate-rank would help.

- Now that determinization exists, create the nash solver and recursive evaluation
  (when both players have perfect information) — determinized worlds are the leaves
  it evaluates.
  - [ ] Then move on to actual bot creation, battle and mentor pages (Could make these an option in the simulate / tracker page?).
