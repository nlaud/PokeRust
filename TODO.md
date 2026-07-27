# TODO: Always remove items from here when they are completed :)

### Fixes
Tracker improvements:

- [ ] Eliminate the remaining tracker-fuzz Illusion identity self-heals. In
  real-team round trips, a physical slot can transiently be promoted to
  `ZoroarkHisui`; later moves that only the disguise species learns trigger
  caught `species cannot learn revealed move` contradictions before the
  conservative recovery path widens the belief. The pipeline now completes,
  but these panic-hook diagnostics show avoidable identity churn. Reproduce with
  `POKERUST_TRACKER_FUZZ_REAL_TEAMS=1`,
  `POKERUST_TRACKER_FUZZ_SEED_START=110018`, and
  `POKERUST_TRACKER_FUZZ_ITERS=1`. The fix must preserve the newest-generation
  rule that direct move damage ends Illusion while indirect damage does not.
- [ ] Make tracker guaranteed-entry-effect synthesis robust to boost-state
  uncertainty. If the concrete target is already at the -6 floor but the
  belief has lost/widened that boost history, a bare Intimidate reveal invents
  an extra `BoostChanged`; the symmetric +6 ceiling can affect reactive boosts.
  Representative seeds from the 110000..120000 real-team sweep are `110143`,
  `110204`, and `119973`. Either retain exact observable boost history across
  every switch/forced-switch shape or extend tracker syntax so an explicitly
  observed no-effect/target list suppresses belief-derived extras.

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
