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
- Sand Spit — summon sandstorm when struck by a move
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

### Entry hazards — setters and removal
Major architectural feature: side condition layers on the field, evaluated when
a Pokémon switches in. Implement all setters and removers together.
**Setters:**
- Spikes — up to 3 layers; flat damage on switch-in
- Stealth Rock — typed damage on switch-in
- Sticky Web — −1 Speed on switch-in
- Toxic Spikes — 1 layer: poison; 2 layers: badly poison on switch-in
- Ceaseless Edge — lays a Spikes layer as a side effect on hit
- Stone Axe — lays a Stealth Rock layer as a side effect on hit
**Removal:**
- Defog — −1 evasion + clear most side conditions and terrain
- Mortal Spin — remove user's-side hazards + poison targets hit
- Rapid Spin — remove user's-side hazards + +1 Speed to user
- Tidy Up — remove all hazards and substitutes field-wide; +1 Atk/Spe to user

### Protect variants
All share the stalling-move mechanic and consecutive-use probability decay (×1/3 per use).
- Protect / Detect — block all moves
- Baneful Bunker — block + poison any contact attacker
- Endure — holder survives this turn at 1 HP regardless
- King's Shield — block + −1 Attack to contact attackers; triggers Stance Change
- Quick Guard — block priority moves for the whole side this turn
- Spiky Shield — block + deal 1/8 max HP to contact attackers
- Wide Guard — block multi-target moves for the whole side this turn

### Forced-switch moves
Force the target to switch out. Shares infrastructure with self-switch (already implemented).
- Circle Throw / Dragon Tail — deal damage then force target to switch
- Roar / Whirlwind — force target to switch (Roar is non-damaging)

### Binding and trapping
Give the target a Bound volatile (chip damage per turn + can't escape for 4–5 turns)
or a Can't Escape volatile (no chip damage).
**Binding (damage + trap):**
- Bind, Fire Spin, Infestation, Sand Tomb, Snap Trap, Wrap
- Whirlpool — also ×2 power vs. a Submerged target
**Trapping only:**
- Block, Mean Look, Spirit Shackle

### Healing — per-turn volatile moves
Give the user (or target) a volatile that restores HP at end of each turn.
- Aqua Ring — restore 1/16 max HP per turn to user
- Ingrain — restore 1/16 max HP per turn; roots user (can't switch out)
- Leech Seed — drain 1/8 of target's max HP per turn; heal user by that amount
- Wish — restore ½ of user's max HP to the Pokémon in that slot on the next turn

### Healing — immediate and redistributive
Moves that change HP without a turn-based volatile.
- Heal Bell — cure status conditions of all party members
- Heal Pulse — restore ½ max HP of one target (can target ally)
- Life Dew — restore ¼ max HP to user and all allies
- Pain Split — average the user's and target's current HP
- Roost — restore ½ max HP; user loses Flying type for the rest of this turn
- Strength Sap — restore HP equal to target's Attack; lower target's Atk by 1

### Volatile status — move restriction
Give the target a volatile that limits which moves it can use. Each requires a
new VolatileStatus variant and end-of-turn countdown.
- Encore — force target to repeat last move (3 turns)
- Taunt — only damaging moves allowed (3 turns)
- Throat Chop — can't use sound-based moves (2 turns)
- Torment — can't use the same move twice in a row

### Volatile status — ongoing debuffs
Volatiles that apply recurring damage, debuffs, or delayed effects each turn.
- Perish Song — all on-field Pokémon faint after 3 turns
- Psychic Noise — Healing Prevented (2 turns)
- Salt Cure — 1/8 HP per turn; ×2 for Water- and Steel-types
- Syrup Bomb — Speed drops 1 stage each turn for 3 turns
- Uproar — user attacks for 3 turns; no Pokémon on field can sleep
- Yawn — target falls asleep at end of next turn

### Stat manipulation — clearing and copying
Remove or duplicate stat-stage changes on the field.
- Clear Smog — deal damage + reset target's stat changes to zero
- Haze — clear all stat changes for all Pokémon on the field
- Psych Up — copy all of the target's current stat changes to user

### Stat manipulation — splitting and swapping
Exchange or average stats or stat changes between Pokémon.
- Guard Split — average Defense and Sp. Def between user and target
- Guard Swap — exchange Defense and Sp. Def changes with target
- Power Shift / Power Trick — swap user's own Attack and Defense stats
- Power Split — average Attack and Sp. Atk between user and target
- Power Swap — exchange Attack and Sp. Atk changes with target
- Speed Swap — swap Speed stats with target

### Stat boosting moves with a cost or condition
Self-boost moves that require paying HP or meeting a condition.
- Belly Drum — Attack → +6; cost ½ max HP
- Charge — +1 Sp. Def; gain Electric Boost status (next Electric move ×2)
- Clangorous Soul — +1 to all stats; cost 1/3 max HP
- Dragon Cheer — +1 crit ratio to allies (+2 if Dragon-type)
- Focus Energy — +2 critical-hit ratio stages
- Magnetic Flux — +1 Def/Sp. Def to Plus/Minus ability allies
- Minimize — +2 evasiveness; gain Minimized status
- Stockpile — +1 Def/Sp. Def; raise Stockpile level (max 3)
- Spit Up — power 100/200/300 per Stockpile level; fails without Stockpile
- Swallow — heal ¼/½/full HP per Stockpile level; fails without Stockpile

### Self-fainting moves
User faints as part of the move. The replacement-choice flow (already built for
forced faint and self-switch) handles bringing in the next Pokémon.
- Explosion / Self-Destruct — high base power; user faints immediately
- Final Gambit — deal damage equal to user's current HP; user faints
- Healing Wish — user faints; replacement enters fully healed with no status
- Memento — −2 Atk and Sp. Atk to target; user faints
- Misty Explosion — user faints; 1.5× power in Misty Terrain

### Crash-damage moves and rampaging moves
**Crash damage (miss or fail → user takes ½ max HP recoil):**
- Axe Kick — also 30% confuse on hit
- High Jump Kick
- Supercell Slam — also ×2 power vs. Minimized targets

**Rampaging (lock into move for 2–3 turns; confused after):**
- Outrage, Petal Dance, Raging Fury, Thrash

### Counter and retaliation moves
Deal damage proportional to damage received this turn. Requires tracking damage
taken this turn by type (physical vs. special).
- Counter — 2× the physical damage received this turn
- Mirror Coat — 2× the special damage received this turn
- Comeuppance / Metal Burst — 1.5× any damage received this turn

### Ability manipulation moves
Change a Pokémon's ability mid-battle.
- Entrainment — change target's ability to match user's
- Gastro Acid — give target the No Ability status
- Role Play — change user's ability to match target's
- Simple Beam — change target's ability to Simple
- Skill Swap — swap user's and target's abilities
- Worry Seed — change target's ability to Insomnia

### Type-changing moves
Alter a Pokémon's active type(s) during battle.
- Electrify — target's next move becomes Electric-type this turn
- Forest's Curse — add Grass type to target
- Magic Powder — change target's type to Psychic
- Reflect Type — change user's type to match target's
- Soak — change target's type to Water
- Trick-or-Treat — add Ghost type to target

### Item manipulation moves
Steal, swap, remove, or force consumption of held items.
- Bug Bite / Pluck — eat target's Berry and apply its effect to user
- Corrosive Gas — all Pokémon on the field lose their held item
- Covet / Thief — steal target's item if user is empty-handed
- Fling — throw held item at target; power and effect depend on the item
- Knock Off — 1.5× power if target holds an item; target loses its item
- Recycle — recover the most recent item the user consumed
- Switcheroo / Trick — swap held items with the target
- Teatime — all Pokémon on the field eat their held Berry

### Side and field condition moves
Set multi-turn conditions on one or both sides of the field.
- Aurora Veil — side condition 5 turns (snow only); halve physical and special damage
- Brick Break / Psychic Fangs / Raging Bull — damage + remove screens on target's side
- Fairy Lock — all Pokémon gain Can't Escape for 1 turn
- Gravity — 5 turns; grounds all Pokémon, raises accuracy, disables certain moves
- Safeguard — side condition 5 turns; protect ally side from status conditions

### Two-turn charging moves (incomplete)
Already partially implemented (Fly, Dig, Dive work). Remaining variants:
- Beak Blast — charge turn: burn any Pokémon that makes contact; attack turn: Flying
- Phantom Force — charge turn: Concealed (untargetable); attack turn: hits through Protect

### Delayed and turn-order manipulation moves
- After You — target moves immediately after user this turn
- Future Sight — hits target's slot 2 turns later (not blocked by current Pokémon's ability)
- Quash — force target to act last this turn

### Complex moves — battle-state conditions
These check or modify ongoing battle state. Each is individually moderate in complexity.
- Acupressure — +2 to a random stat (user or random ally in doubles)
- Belch — 120-power; fails unless user has eaten a Berry this battle
- Body Slam — 30% paralyze; ×2 power and never misses vs. Minimized target
- Burn Up — 130-power Fire move; user loses their Fire type after use
- Copycat — use the most recent move used on the field
- Darkest Lariat / Foul Play / Sacred Sword — ignore target's stat stages in damage calc
- Endeavor — deal damage equal to (target's current HP − user's current HP); min 1
- Fell Stinger — if this KOs the target, user's Attack rises by 3
- Fickle Beam — 30% chance to double this move's power
- Freeze-Dry — Ice-type, but also super effective against Water types
- Gigaton Hammer — 160 power; cannot be selected twice in a row
- Grav Apple — ×1.5 power in Gravity; also lower target's Defense by 1
- Grassy Glide — gains +1 priority while Grassy Terrain is active
- Helping Hand — boost an ally's move power by 50% this turn
- Lock-On — user's next move is guaranteed to hit
- Poltergeist — 110 power; fails if the target holds no item
- Snore — only usable while asleep; 30% flinch on hit
- Sparkling Aria — deal damage; cure any burn on targets hit
- Spite — remove 4 PP from the target's most recently used move
- Steel Beam — 140 power; user takes ½ max HP as recoil
- Stuff Cheeks — eat held Berry; +2 Defense
- Sucker Punch — priority move; fails if target did not choose a damaging move this turn
- Super Fang — deal damage equal to ½ of target's current HP (min 1)
- Tearful Look — −1 Atk and Sp. Atk to target; ignores evasion; hits through Protect
- Upper Hand — flinch target; fails if target is not about to use a priority move

### Complex moves — transformation, substitution, and doubles
More architecturally significant; each likely needs its own research pass.
- Ally Switch — user swaps field position with an ally; success rate degrades each use
- Destiny Bond — if user faints from an opponent's move this turn, that opponent also faints
- Dragon Darts — two hits; splits between two opponents when both are present
- Eerie Spell — deal damage + remove 3 PP from target's last-used move
- Fake Out / First Impression — only usable on the very first turn after entering battle
- Feint — hits through and removes Protect/Detect for this turn
- Flying Press — damage uses both Fighting and Flying type effectiveness combined
- Instruct — make the target immediately repeat their last move
- Last Resort — 140 power; fails unless the user has already used all other known moves
- Misty Explosion — ×1.5 power in Misty Terrain; user faints
- Pollen Puff — deal damage to opponents; restore ½ max HP to allies instead
- Round — power doubles for each additional Round user acting in the same turn
- Shell Side Arm — use whichever calculation (physical or special) deals more damage; 20% poison
- Smack Down — hits airborne targets; give them Landed status (grounded, loses Flying immunity)
- Substitute — lose ¼ max HP to create a substitute that absorbs incoming damage
