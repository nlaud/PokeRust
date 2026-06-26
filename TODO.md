# TODO

---
## Information Abilities
- Illusion — enter disguised as the last party member
- Anticipation — signal if opponents have SE or OHKO moves (message-only; no battle-state change in a full-information sim)
- Frisk — reveal an opponent's held item (message-only; no battle-state change in a full-information sim)

## simulate_turn Event Emission (Phase 2)
Phase 1 wired the top-level wrappers (MoveUsed, Switch, Mega, Tera, EndOfTurn, DamageDealt,
Faint, Weather/Terrain/PseudoWeather/SideCondition changed). Still to emit:

**Remaining Phase 1 sites (straightforward wiring):**
- `Healed` — drain/leech callers in `apply_post_damage_move_effects`; EoT heal callers in `end_turn`
- `SetHp` — Pain Split site
- `StatusInflicted` / `StatusCured` — callers of `apply_status_to_pokemon`; inline `mon.status = None` cure sites (~10)
- `BoostChanged` — callers of `apply_boosts_returning_delta` (emit one event per nonzero index of returned delta)
- `VolatileStart` / `VolatileEnd` — callers of `apply_volatile_to_pokemon` / `remove_status_volatile`
- `SlotConditionStart` / `SlotConditionEnd` — slot-condition write sites (`resolve_wish_slot_conditions`, etc.)
- `ItemLost` / `ItemGained` / `ItemRevealed` — `process_item_loss_events`; Knock Off / theft sites
- `WeatherChanged { weather: None }` / `TerrainChanged { terrain: None }` — expiry in `decrement_effect_timers`

**Phase 2 (scattered qualifiers):**
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
