# TODO: Always remove items from here when they are completed :)

### Fixes
Tracker improvements:

- [ ] Fix target attribution for guaranteed multi-target move effects. The
  simulator emits String Shot's `spe -2` on its user even though the move's
  `MoveUsed.targets` are both opponents; tracker synthesis correctly applies
  the drops to those opponents, so the observable multisets diverge. Reproduce
  with real-team tracker-fuzz seed `113241` (turn 7). Audit other top-level
  `boosts:` moves with `allAdjacentFoes`; several seeds in `113000..114000`
  report the same multiset family.
- [ ] Do not synthesize end-of-turn timer expiry after a terminal KO. Seed
  `113057` ends with both P1 actives fainting before the engine runs `end_turn`,
  but tracker augmentation appends `SideConditionEnd { P2, TailWind }` to its
  parser-required `EndOfTurn` sentinel. The current healthy-reserve gate can be
  fooled by stale/duplicate `known_back` entries.
- [ ] Repair item identity across switch/faint bucket moves. Real-team
  tracker-fuzz seed `113540` eventually assigns `SafetyGoggles` and then
  `LifeOrb` to `mon#2`, causing an `ItemRevealed` contradiction. Confirm whether
  the stale identity originates in synthesis scratch or core inference before
  widening any item constraint.

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
