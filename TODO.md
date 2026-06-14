# TODO

Entries are grouped into session-sized batches. Each group shares a common implementation
hook and should be researchable, plannable, and implementable in a single focused session.

---
## Saved for later (Information only)
- Illusion — enter disguised as the last party member
- Anticipation — signal if opponents have SE or OHKO moves (message-only; no battle-state change in a full-information sim)
- Frisk — reveal an opponent's held item (message-only; no battle-state change in a full-information sim)


## Abilities

### Complex abilities — mechanics modifiers
Each has significant individual complexity. Research all edge cases carefully.
- Contrary — all stat stage changes are inverted
- Early Bird — halve sleep turn counter
- Heavy Metal / Light Metal — double or halve holder's weight for weight-based moves
- Infiltrator — bypass Light Screen, Reflect, Aurora Veil, Safeguard, and Substitute
- Innards Out — when KO'd by a move, deal the damage that brought HP to 0 back to attacker
- Inner Focus / Oblivious / Own Tempo / Scrappy — Intimidate immunity + specific extra
- Magic Bounce — reflect status moves back at the user
- Mega Sol — moves always act as if in harsh sunlight
- Mimicry — change type to match the active terrain
- Minus / Plus — +50% Sp. Atk when an ally with the opposite ability is present
- Mold Breaker / Turboblaze / Teravolt — moves ignore the target's ability
- Parental Bond — attack twice; second hit at ¼ power
- Pressure — opponents expend 1 extra PP per move used against the holder
- Protean / Libero — change type to match the move being used (once per switch-in)
- Sand Force — +30% power for Rock/Ground/Steel in sandstorm
- Shadow Tag — opponents cannot switch out (does not affect other Shadow Tag users)
- Sheer Force — remove secondary effects from moves; gain 1.3× power on those moves
- Stalwart — ignore move and ability redirection
- Stench — 10% flinch chance on any damaging hit
- Sturdy — survive any OHKO at full HP; immune to one-hit KO moves
- Super Luck — +1 critical-hit ratio stage
- Synchronize — spread burn, paralysis, or poison back to the Pokémon that inflicted it
- Toxic Debris — set Toxic Spikes on opponent's side when holder is hit by a physical move
- Unaware — ignore target's stat changes when attacking; ignore attacker's when defending

---

## Moves

### Complex moves — battle-state conditions
These check or modify ongoing battle state. Each is individually moderate in complexity.
- Tearful Look — −1 Atk and Sp. Atk to target; ignores evasion; hits through Protect

### Complex moves — transformation, substitution, and doubles
More architecturally significant; each likely needs its own research pass.
- Ally Switch — user swaps field position with an ally; success rate degrades each use
- Destiny Bond — if user faints from an opponent's move this turn, that opponent also faints
- Dragon Darts — two hits; splits between two opponents when both are present
- Eerie Spell — deal damage + remove 3 PP from target's last-used move
- Feint — hits through and removes Protect/Detect for this turn
- Flying Press — damage uses both Fighting and Flying type effectiveness combined
- Instruct — make the target immediately repeat their last move
- Last Resort — 140 power; fails unless the user has already used all other known moves
- Pollen Puff — deal damage to opponents; restore ½ max HP to allies instead
- Round — power doubles for each additional Round user acting in the same turn
- Shell Side Arm — use whichever calculation (physical or special) deals more damage; 20% poison
- Smack Down — hits airborne targets; give them Landed status (grounded, loses Flying immunity)
- Substitute — lose ¼ max HP to create a substitute that absorbs incoming damage

## Refactors
- use thaws_target move bool and defrost move flag instead of fixed lists.
- Use move flags such as IgnoreImmunity, IgnoreEvasion, Ignore... Flags (Refactor these to be move flags). Use all other move flags as well.
- Recheck all test, suggest new tests, etc.
- Refactor Outrage-type moves to act more like sleep, having a 50% chance to end after 2nd attack, then 100% on the last one.
