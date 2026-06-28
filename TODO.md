# TODO

---
## Information Abilities
- Illusion — enter disguised as the last party member
- Anticipation — signal if opponents have SE or OHKO moves (message-only; no battle-state change in a full-information sim)
- Frisk — reveal an opponent's held item (message-only; no battle-state change in a full-information sim)

## simulate_turn Event Emission
Phase 1 ✅ complete — all top-level wrappers and leaf sites wired.
Phase 2 ✅ complete — all scattered qualifiers emitted: `Crit`, `HitCount`, `Immune`, `MoveFailed`,
`Blocked`, `Missed`, `Cant`, `BoostsCleared/Inverted/Swapped/Copied`, `AbilityRevealed`,
`FormeChange`, `TypeChanged`, `ChargingMove`, `MustRecharge`, `SingleMoveOrTurn`, `PerishCount`,
`SimultaneousSwitch`.
Phase 3 ✅ complete — six round-trip tests in `mod event_round_trip` verify nesting, multi-hit
crit (`Crit`/`DamageDealt`/`HitCount`), `Cant` top-level placement, `Blocked` nesting, HP
perspective (`Number` vs `Percent`), and crit-branch non-merging. Also fixed `DamageDealt`
not being emitted for standard opponent hits (`apply_single_hit_branch` now emits after the
`take_damage` call once the `target_mon` borrow ends).

## Refactors
- Speed Investigation
- Comments Deslop
- MAKE THE FRONTEND YIPPEE
  - Start with Teams, Simulate, and Tracker pages
    - For tracker will need a parser for lines of input -> action / reaction tree (figuring out what is a reaction to what and what causes what from just the lines, also must add guaranteed effect so the user doesn't need to put those in manually). THis will likely include some RegEx stuff as well. Need a detailed spec.
    - https://stitch.withgoogle.com/projects/6512361286860616575 for designs
    - https://github.com/PokeAPI/sprites for FE sprites
  - Then move on to actual bot creation, battle and mentor pages.
