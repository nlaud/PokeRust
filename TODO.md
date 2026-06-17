# TODO

Entries are grouped into session-sized batches. Each group shares a common implementation
hook and should be researchable, plannable, and implementable in a single focused session.

---
## Saved for later (Information only)
- Illusion — enter disguised as the last party member
- Anticipation — signal if opponents have SE or OHKO moves (message-only; no battle-state change in a full-information sim)
- Frisk — reveal an opponent's held item (message-only; no battle-state change in a full-information sim)

## Refactors
- Use thaws_target move bool and defrost move flag instead of fixed lists.
- Use move flags such as IgnoreImmunity, IgnoreEvasion, Ignore... Flags (Refactor these to be move flags) instead of dedicated helper functions. Use all other move flags as well.
- Refactor Outrage-type moves to act more like sleep, having a 50% chance to end after 2nd attack, then 100% on the last one.
- Recheck all tests, suggest new tests, verify mechanics.
