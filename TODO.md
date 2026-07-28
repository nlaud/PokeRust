# TODO: Always remove items from here when they are completed :)

### Fixes
Tracker improvements:

- [ ] Model every temporally valid silent-entry path for weather-setting
  abilities, then re-enable weather-setter absence narrowing. It is currently
  disabled conservatively: real-team tracker fuzz found `SnowWarning` could be
  silent on one entry and directly revealed on a later entry even after the
  obvious matching-weather and primordial-weather no-op cases were handled.

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
