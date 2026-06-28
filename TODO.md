# TODO

---
## Information Abilities
- Illusion — enter disguised as the last party member
- Anticipation — signal if opponents have SE or OHKO moves (message-only; no battle-state change in a full-information sim)
- Frisk — reveal an opponent's held item (message-only; no battle-state change in a full-information sim)

## simulate_turn Event Emission (Phase 2)
Phase 1 ✅ complete — all top-level wrappers and leaf sites wired (WeatherChanged, BoostChanged,
ItemLost/Gained/Revealed, StatusInflicted/Cured, VolatileStart/End, SlotConditionStart/End,
Healed, SetHp; including Rest sleep+heal, Smack Down MagnetRise/Telekinesis, berry-cure
status/confusion, Mental Herb volatiles + ItemLost, Fling Mental Herb).

**Phase 2 — scattered qualifiers still to emit:**
- `Crit` — `apply_single_hit_branch` (has `is_crit` + slot)
- `HitCount` — once per move under MoveUsed wrapper; count from `multihit_hit_count_branches`
- `Immune` / `MoveFailed` / `Blocked` / `Missed` — refactor 17 inline `last_move_failed = true` sites into a `note_move_outcome(bs, slot, outcome)` resolver so the EventKind can be emitted at the right level
- `BoostsCleared` / `BoostsInverted` / `BoostsSwapped` / `BoostsCopied` — Haze/Clear Smog/Topsy-Turvy/Heart-Power-Guard Swap/Psych Up sites
- `AbilityRevealed` — `process_pokemon_send_out`; Trace/Mummy/Skill Swap; Intimidate; mega ability change
- `FormeChange` / `TypeChanged` — forme-change callers (Palafin/Aegislash/Castform); Protean/Libero/Soak
- `Cant` — pre-move can't-act checks mapped to `CantReason`
- `ChargingMove` / `MustRecharge` / `SingleMoveOrTurn` — two-turn charge; Hyper Beam recharge; Protect/Detect/Beak Blast
- `PerishCount` — Perish Song tick in `end_turn`
- `SimultaneousSwitch` — wrap `process_sendouts_in_speed_order_branching` + leads in `battle_state_from_preview_branching`

**After Phase 2:** Write round-trip tests using `run_single_turn_with_events` verifying nesting,
multi-hit, perspective (Number vs Percent), and crit-branch non-merging behaviour.

## Refactors
- Speed Investigation
- Comments Deslop
- MAKE THE FRONTEND YIPPEE
  - Start with Teams, Simulate, and Tracker pages
    - For tracker will need a parser for lines of input -> action / reaction tree (figuring out what is a reaction to what and what causes what from just the lines, also must add guaranteed effect so the user doesn't need to put those in manually). THis will likely include some RegEx stuff as well. Need a detailed spec.
    - https://stitch.withgoogle.com/projects/6512361286860616575 for designs
    - https://github.com/PokeAPI/sprites for FE sprites
  - Then move on to actual bot creation, battle and mentor pages.
