# TODO

Entries are grouped by implementation similarity — items in the same group share
the same code pattern and can usually be knocked out together.

---

## Items

### Survival / endure items
Trigger when a hit would KO; keep holder at 1 HP.
- Focus Sash — full HP → survive any one hit (one-time)
- Focus Band — 10% chance to survive any hit (repeatable)

### End-of-turn / on-hit HP recovery items
- Leftovers — restore 1/16 max HP at end of every turn
- Shell Bell — restore HP equal to 1/8 of damage dealt

### Speed / turn-order items
- Choice Scarf — 1.5× Speed stat, but locks the holder to one move
- Quick Claw — 23.4% chance to move first among moves of the same priority

### Crit / flinch chance items
- Scope Lens — +1 critical-hit ratio stage
- King's Rock — 10% chance to flinch the target on any damage-dealing hit

### One-time stat-reset items
Consumed on trigger to undo a negative stat effect.
- White Herb — cure any and all lowered stats
- Mental Herb — cure binding volatile statuses (Infatuated, Taunted, Disabled, etc.)

### Species-specific items
- Light Ball — double Pikachu's Attack and Sp. Atk

---

## Abilities

### Type immunities with stat / HP bonus
Negate an entire move type; grant a bonus instead.
- Earth Eater (Ground → restore 1/4 HP)
- Flash Fire (Fire → boost Fire power by 50%)
- Lightning Rod (Electric → +1 Sp. Atk)
- Motor Drive (Electric → +1 Speed)
- Sap Sipper (Grass → +1 Attack)
- Volt Absorb (Electric → restore 1/4 HP)
- Water Absorb (Water → restore 1/4 HP)

### -ate abilities (Normal → another type + 20% power boost)
- Aerilate (→ Flying)
- Dragonize (→ Dragon)
- Pixilate (→ Fairy)
- Refrigerate (→ Ice)
- Liquid Voice — sound-based moves → Water (no power boost, shares the type-conversion hook)

### On-contact reactive effects
Fire when the holder is hit by a contact move.
- Aftermath — attacker loses 1/4 max HP
- Cute Charm — 30% chance to inflict Infatuated on attacker (opposite gender)
- Cursed Body — 30% chance to disable the move used against the holder
- Flame Body — 30% chance to burn attacker
- Gooey — lower attacker's Speed by 1 stage
- Mummy — overwrite attacker's ability with Mummy
- Poison Point — 30% chance to poison attacker
- Rough Skin — attacker loses 1/8 max HP
- Spicy Spray — burn attacker
- Static — 30% chance to paralyze attacker
- Wandering Spirit — swap abilities with attacker
- Weak Armor — holder: −1 Defense, +2 Speed

### Damage reduction abilities
- Filter / Solid Rock — −25% damage from super-effective moves
- Friend Guard — allies take 25% less damage
- Fur Coat — halve damage from physical moves
- Heatproof — halve damage from Fire moves and halve burn damage
- Multiscale — halve damage received at full HP
- Purifying Salt — halve damage from Ghost moves; immune to status conditions
- Thick Fat — halve damage from Fire and Ice moves
- Water Bubble — halve Fire damage; double Water power; cannot be burned

### Low-HP emergency type boosts (≤ 1/3 HP → 1.5× for one type)
- Blaze (Fire)
- Overgrow (Grass)
- Swarm (Bug)
- Torrent (Water)

### Stat protection abilities
Prevent stat reduction from other Pokémon's moves or abilities.
- Big Pecks (Defense only)
- Clear Body (all stats)
- Hyper Cutter (Attack only)
- White Smoke (all stats)

### Priority manipulation abilities
- Armor Tail / Queenly Majesty — opponents can't use priority moves against the holder's side
- Gale Wings — Flying moves get +1 priority while holder's HP is full
- Prankster — status moves get +1 priority
- Quick Draw — 30% chance to act first among moves of the same priority
- Stall — holder's moves always go last among moves of the same priority

### Entry effects
Trigger once when the Pokémon switches into battle.
- Anticipation — signal if opponents have SE or OHKO moves
- Curious Medicine — remove all stat changes from allies
- Frisk — reveal an opponent's held item
- Hospitality — restore 1/4 of ally's max HP
- Illusion — enter disguised as the last party member
- Imposter — transform into the opposing Pokémon (copies stats except HP)
- Screen Cleaner — remove Light Screen, Reflect, and Aurora Veil from both sides
- Supersweet Syrup — lower all opponents' evasiveness by 1 (once per battle)
- Supreme Overlord — +10% move power per fainted party member, max +50%
- Trace — copy an opponent's ability

### End-of-turn effects
- Harvest — 50% chance (100% in sun) to regenerate a consumed Berry
- Healer — 50% chance to cure an ally's status condition
- Hunger Switch — toggle between Full Belly and Hangry form
- Moody — +2 to a random stat, −1 to another
- Shed Skin — 30% chance to cure the holder's own status condition
- Speed Boost — +1 Speed stage

### Berry-interaction abilities
- Cheek Pouch — restore an extra 1/3 max HP when eating a Berry
- Cud Chew — eat the same Berry again at the end of the next turn
- Gluttony — trigger HP-threshold Berries at ≤50% HP instead of ≤25%
- Ripen — double the effects of Berries eaten by the holder
- Unnerve — opponents cannot eat Berries

### Stat-change reaction abilities
Trigger when stats are raised, lowered, or a threshold is crossed.
- Anger Point — take a critical hit → Attack goes to +6
- Berserk — HP drops to ≤50% from a hit → +1 Sp. Atk
- Competitive — an opponent lowers any stat → +2 Sp. Atk
- Defiant — an opponent lowers any stat → +2 Attack
- Electromorphosis — take damage from a move → gain Electric Boost status
- Justified — take damage from a Dark move → +1 Attack
- Moxie — knock out a target → +1 Attack
- Opportunist — an opponent boosts stats → mirror those exact boosts
- Stamina — take damage from a move → +1 Defense
- Steadfast — flinch → +1 Speed

### Move power multipliers
- Analytic — 1.3× power when acting last in the turn
- Huge Power / Pure Power — double the power of physical moves
- Hustle — 1.5× Attack; physical move accuracy ×0.8
- Iron Fist — 1.2× power for punching moves
- Mega Launcher — 1.5× power for pulse moves
- Rivalry — 1.25× vs. same gender; 0.75× vs. opposite gender
- Sharpness — 1.5× power for slicing moves
- Strong Jaw — 1.5× power for biting moves
- Technician — 1.5× power for moves with base power ≤ 60
- Tough Claws — 1.3× power for contact moves

### Immunity / move-blocking abilities
Block entire categories of moves or secondary effects.
- Aroma Veil — holder and allies cannot gain mental volatile statuses (Infatuated, Taunted, etc.)
- Bulletproof — immune to ball and bomb moves
- Damp — all Pokémon unable to use explosive moves; explosive abilities fail
- Flower Veil — Grass-type allies immune to status conditions and stat drops
- Illuminate / Keen Eye — ignore evasiveness changes; accuracy cannot be lowered
- Levitate — immune to Ground moves, Spikes, Toxic Spikes, and Sticky Web
- Magic Guard — take damage only from direct attacks
- Overcoat — immune to sandstorm damage and powder/spore moves
- Soundproof — immune to sound-based moves
- Sweet Veil — holder and allies cannot become drowsy or be put to sleep
- Shield Dust — immune to secondary effects from moves
- Telepathy — dodge moves used by allies

### Ability copying / spreading
- Mummy — overwrite attacker's ability on contact (see also on-contact group)
- Receiver — inherit the ability of a knocked-out ally
- Trace — copy an opponent's ability on entry (see also entry group)
- Wandering Spirit — swap abilities with attacker on contact

### Form-change abilities
- Disguise — absorb first hit (lose 1/8 HP) → change to Busted Form
- Forecast — transform type to match weather (Water/Fire/Ice)
- Hunger Switch — alternate Full Belly ↔ Hangry at end of every turn
- Stance Change — Blade Forme when using an attack; Shield Forme when using King's Shield
- Zero To Hero — switch out of battle → permanently become Hero Form

### Contact-move pass-through
- Piercing Drill / Unseen Fist — contact moves deal 1/4 damage through Protect
- Long Reach — holder's moves are never treated as contact moves

### Item-interaction abilities
- Klutz — held items have no effect on the holder
- Magician — steal target's held item when dealing damage (if empty-handed)
- Pickpocket — steal attacker's held item when hit by a contact move (if empty-handed)
- Pickup — pick up an item consumed by any Pokémon that turn (if empty-handed)
- Sticky Hold — held item cannot be stolen or knocked off
- Symbiosis — give own item to an ally that just consumed theirs
- Unburden — ×2 Speed when the held item is consumed or lost

### Complex / unique abilities
- Contrary — stat changes are inverted (boosts become drops and vice versa)
- Early Bird — wake from sleep in half the usual turns
- Fairy Aura — boosts the power of all Fairy-type moves on the field by 1.33×
- Heavy Metal / Light Metal — double or halve the holder's weight
- Infiltrator — bypass Light Screen, Reflect, Aurora Veil, Safeguard, and substitutes
- Innards Out — when knocked out, deal the same damage to the attacker
- Inner Focus / Oblivious / Own Tempo / Scrappy — Intimidate immunity plus a specific additional effect (no flinch / no Taunt or Infatuation / no confusion / Normal+Fighting hit Ghosts)
- Magic Bounce — reflect status moves back at the user
- Mega Sol — moves act as if under harsh sunlight regardless of actual weather
- Mimicry — change type to match the current terrain
- Minus / Plus — +50% Sp. Atk if an ally with Plus or Minus is on the field
- Mirror Armor — bounce back any stat-lowering effects to their source
- Mold Breaker — moves ignore the target's ability
- Parental Bond — attack twice; second hit deals 1/4 power
- Pressure — opponents expend 1 extra PP per move used against the holder
- Protean — change type to match the move being used (once per switch-in)
- Sand Force — +30% power for Rock, Ground, and Steel moves in sandstorm
- Sand Spit — summon a sandstorm when hit by a move
- Shadow Tag — opponents cannot switch out
- Sheer Force — remove secondary effects from moves; gain 1.3× power on those moves
- Stalwart — ignore moves and abilities that redirect targets
- Stench — 10% chance to cause flinch when dealing damage
- Sturdy — survive any OHKO at full HP; immune to one-hit KO moves
- Super Luck — +1 critical-hit ratio stage
- Synchronize — spread burn, paralysis, or poison back to the Pokémon that inflicted it
- Toxic Debris — set Toxic Spikes on the opponent's side when hit by a physical move
- Unaware — ignore the target's stat boosts when attacking; ignore the attacker's when defending

---

## Moves

### Entry hazard setters
Place effect layers on the opponent's side of the field.
- Spikes — up to 3 layers; damage switch-ins
- Stealth Rock — deals typed damage to switch-ins
- Sticky Web — lower Switch-in Speed by 1
- Toxic Spikes — poison (or badly poison with 2 layers) switch-ins
- Ceaseless Edge — Spikes layer as a side effect on hit
- Stone Axe — Stealth Rock layer as a side effect on hit

### Entry hazard removal
- Defog — −1 evasion + remove most side conditions + terrain
- Mortal Spin — remove user's-side hazards + poison targets hit
- Rapid Spin — remove user's-side hazards + +1 Speed
- Tidy Up — remove hazards and substitutes field-wide + +1 Atk/Spe to user

### Protect variants
All share the consecutive-use probability decay (×1/3 per use in a row).
- Protect / Detect — pure protection
- Baneful Bunker — poison contact attackers
- King's Shield — −1 Attack on contact attackers; triggers Stance Change
- Spiky Shield — deal 1/8 max HP damage to contact attackers
- Endure — holder survives this turn at 1 HP no matter what
- Quick Guard — block priority moves for the whole side
- Wide Guard — block multi-target moves for the whole side

### Pivot / forced-switch moves
Allow or force a switch during or after the move.
- Circle Throw / Dragon Tail — deal damage; force target to switch out
- Roar / Whirlwind — force target to switch out (Roar non-damaging)

### Binding moves
Give the target the Bound status (chip damage, trapped for 4–5 turns).
- Bind, Fire Spin, Infestation, Sand Tomb, Snap Trap, Wrap
- Whirlpool — also doubles in power against a Submerged target

### HP-scaled variable power
Power is a function of remaining HP or a stat comparison.
- Eruption / Water Spout — user's HP% → 1–150
- Flail / Reversal — user's HP% → 20–200
- Gyro Ball — target Spe vs user Spe → 1–150
- Electro Ball — user Spe vs target Spe → 40–150
- Hard Press — target's HP% → 1–100
- Grass Knot / Low Kick — target's weight → 20–120
- Heat Crash / Heavy Slam — weight ratio (user/target) → 40–120

### One-hit KO and fixed-damage moves
- Fissure / Guillotine / Horn Drill — 30% OHKO; fails if user's level < target's
- Sheer Cold — 30% OHKO (20% for non-Ice-type users); fails vs. Ice types
- Night Shade / Seismic Toss — deal damage equal to user's level (50 in standard play)

### Counter / retaliation moves
Deal damage proportional to damage received this turn.
- Counter — 2× the physical damage taken
- Mirror Coat — 2× the special damage taken
- Comeuppance / Metal Burst — 1.5× any damage taken

### Conditionally scaled power
Power is multiplied (usually ×2) under a specific battle condition.
- Acrobatics — ×2 if user holds no item
- Assurance — ×2 if target already took damage this turn
- Avalanche — ×2 if target dealt damage to the user this turn
- Burning Jealousy — ×2 if target had a stat raised this turn
- Hex / Infernal Parade — ×2 if target has a status condition
- Lash Out — ×2 if the user's stats were lowered this turn
- Payback — ×2 if user acts after the target
- Stomping Tantrum / Temper Flare — ×2 if user's last move failed or missed
- Venoshock — ×2 if target is poisoned or badly poisoned
- Power Trip / Stored Power — base 20 + 20 per +1 boost stage the user has
- Last Respects — base 50 + 50 per fainted party member

### Self-fainting moves
User faints as part of the move's effect.
- Explosion / Self-Destruct — high base power; user faints
- Final Gambit — deals damage equal to user's current HP; user faints
- Healing Wish — user faints; the Pokémon that replaces it is fully healed + status cured
- Memento — −2 Atk/Sp. Atk to target; user faints
- Misty Explosion — 1.5× power in Misty Terrain; user faints

### Crash-damage moves (fail / miss → recoil)
- High Jump Kick — miss or fail → user takes 1/2 max HP
- Axe Kick — miss or fail → user takes 1/2 max HP; also 30% confuse on hit
- Supercell Slam — miss or fail → user takes 1/2 max HP; ×2 vs Minimized

### Rampaging moves
User gains Rampaging status, attacks 2–3 turns involuntarily, then becomes confused.
- Outrage, Petal Dance, Raging Fury, Thrash

### Ability manipulation moves
- Entrainment — change target's ability to match user's
- Gastro Acid — give target the No Ability status
- Role Play — change user's ability to match target's
- Simple Beam — change target's ability to Simple
- Skill Swap — swap user's and target's abilities
- Worry Seed — change target's ability to Insomnia

### Type-changing moves
Alter a Pokémon's type(s) during battle.
- Electrify — target's move becomes Electric-type this turn
- Forest's Curse — add Grass type to target
- Magic Powder — change target's type to Psychic
- Reflect Type — change user's type to match target's
- Soak — change target's type to Water
- Trick-or-Treat — add Ghost type to target

### Item manipulation moves
Steal, swap, remove, or consume held items.
- Bug Bite / Pluck — eat the target's Berry and gain its effect
- Corrosive Gas — all Pokémon on the field lose their held items
- Covet / Thief — steal the target's item if user is empty-handed
- Fling — throw user's held item at target; effect and power depend on item
- Knock Off — 1.5× power if target holds an item; target loses the item
- Recycle — recover the last item the user consumed
- Switcheroo / Trick — swap held items with the target
- Teatime — all Pokémon on the field eat their held Berries

### Healing and HP redistribution moves
- Aqua Ring — restore 1/16 max HP per turn (Aqua Ring volatile)
- Heal Bell — cure the status of all party members including the user
- Heal Pulse — restore 1/2 of target's max HP (can target ally)
- Ingrain — restore 1/16 max HP per turn; roots user (can't switch)
- Leech Seed — drain 1/8 target's max HP per turn; heal user by that amount
- Life Dew — restore 1/4 max HP to user and all allies
- Pain Split — average the user's and target's current HP
- Roost — restore 1/2 max HP; lose Flying type for the rest of this turn
- Strength Sap — restore HP equal to target's Attack stat; lower target's Atk by 1
- Wish — restore 1/2 of user's max HP to whatever is in that slot next turn

### Two-turn / charging moves
Take a "charge" turn before attacking.
- Beak Blast — charge turn: any contact attacker is burned; attack turn: normal
- Fly — charge turn: Sky-High status; attack turn: Flying move
- Phantom Force — charge turn: Concealed (untargetable); attack turn: hits through Protect
- Future Sight — move resolves 2 turns later on the target's spot

### Side / field condition moves
- Aurora Veil — side condition for 5 turns (snow only); halve physical and special damage
- Brick Break / Psychic Fangs / Raging Bull — deal damage + remove screens on target's side
- Fairy Lock — all Pokémon on field gain Fairy Locked (can't switch) for 1 turn
- Gravity — field status for 5 turns; grounds all Pokémon, raises accuracy, disables certain moves
- Grassy Terrain — field terrain for 5 turns (see also Grassy Glide below)
- Safeguard — side condition for 5 turns; protect from status conditions

### Volatile status infliction
Give the target a volatile condition affecting their actions.
- Attract — Infatuated (50% chance to not act; opposite gender only)
- Disable — Move Disabled on the last move used (4 turns)
- Encore — force target to repeat last move (3 turns)
- Imprison — Sealing Off (opponents can't use moves the user knows)
- Perish Song — all on-field Pokémon faint after 3 turns
- Psychic Noise — Healing Prevented for 2 turns
- Salt Cure — Salt Cured (1/8 HP per turn, doubled for Water/Steel)
- Syrup Bomb — Syrupy for 3 turns (Speed drops 1 each turn)
- Taunt — Taunted; only damaging moves allowed (3 turns)
- Throat Chop — Throat Chopped; can't use sound moves (2 turns)
- Torment — Unable to Repeat (can't use same move twice in a row)
- Uproar — Uproar status; user attacks 3 turns; no one can sleep
- Yawn — Drowsy; falls asleep at the end of next turn

### Trapping moves
Give the target the Can't Escape status.
- Block, Mean Look, Spirit Shackle

### Stat boosting moves (user / ally)
- Acupressure — +2 to a random stat (user or ally)
- Belly Drum — set Attack to +6; cost 1/2 max HP
- Charge — +1 Sp. Def; gain Electric Boost status (next Electric move ×2)
- Clangorous Soul — +1 to all stats; cost 1/3 max HP
- Dragon Cheer — +1 or +2 crit ratio to allies (Dragon types get +2)
- Focus Energy — +2 critical-hit ratio stages
- Magnetic Flux — +1 Def/Sp. Def to Plus and Minus ability allies
- Minimize — +2 evasiveness; gain Minimized status (double damage from certain moves)
- Stockpile — +1 Def/Sp. Def; raise Stockpile level (max 3)

### Stat swap / split / copy moves
- Clear Smog — deal damage; reset target's stat changes
- Guard Split — average Defense and Sp. Def between user and target
- Guard Swap — exchange Defense and Sp. Def changes with target
- Haze — clear all stat changes on the entire field
- Power Shift / Power Trick — swap user's own Attack and Defense stats
- Power Split — average Attack and Sp. Atk between user and target
- Power Swap — exchange Attack and Sp. Atk changes with target
- Psych Up — copy all of the target's current stat changes
- Speed Swap — swap Speed stats with target

### Complex / unique moves
- After You — target moves immediately after the user this turn
- Alluring Voice — confuse the target if its stats were raised this turn
- Ally Switch — user swaps field position with an ally; success rate degrades
- Aura Wheel — +1 Speed; type depends on Morpeko's current form
- Beat Up — hit once per healthy, status-free party member (power = each member's base Atk ÷ 10 + 5)
- Belch — 120-power; fails unless the user has eaten a Berry this battle
- Body Slam — 30% paralyze; ×2 power and never misses vs. Minimized
- Burn Up — 130-power Fire move; user loses their Fire type
- Copycat — use whichever move was last used on the field
- Curse — Ghost type: lose 1/2 HP to inflict Cursed on target; non-Ghost: −1 Spe, +1 Atk, +1 Def
- Darkest Lariat / Foul Play / Sacred Sword — ignore target's stat changes when calculating damage
- Destiny Bond — if holder faints this turn from an opponent's move, that opponent also faints
- Dragon Darts — two hits; splits between two opponents if both are present
- Eerie Spell — deal damage; remove 3 PP from the target's last-used move
- Endeavor — deal damage equal to target's current HP minus user's current HP
- Fake Out / First Impression — only work on the first turn after entering battle
- Feint — hits even through Protect/Detect and removes their effect for this turn
- Fell Stinger — if this KOs the target, user's Attack rises by 3
- Fickle Beam — 30% chance to double this move's power
- Flying Press — damage computed with both Fighting and Flying type effectiveness combined
- Follow Me / Rage Powder — redirect all single-target moves toward user for this turn
- Freeze-Dry — Ice-type move that is also super effective against Water types
- Future Sight — hits the target's slot 2 turns later (not blocked by current Pokémon's ability)
- Gigaton Hammer — 160 power; cannot be selected twice in a row
- Grav Apple — +50% power during Gravity; −1 Defense to target
- Grassy Glide — +1 priority while Grassy Terrain is active
- Helping Hand — boost power of an ally's move by 50% this turn
- Hydro Cannon — 150-power; user gains Recharging status next turn
- Instruct — make the target immediately reuse the last move it used
- Last Resort — 140 power; fails unless the user has already used every other known move
- Lock-On — next move the user uses is guaranteed to hit
- Misty Explosion — 100-power; user faints; 1.5× in Misty Terrain
- Night Shade / Seismic Toss — deal damage equal to user's level (typically 50)
- Pollen Puff — deals damage to opponents; restores 1/2 HP to allies instead
- Poltergeist — 110 power; fails if the target has no held item
- Power Trip / Stored Power — base 20 + 20 per +1 boost stage (same formula, different type)
- Quash — force the target to act last this turn
- Recycle — recover the last held item the user consumed
- Round — power doubles for each additional user of Round in the same turn
- Shell Side Arm — use physical or special calculation, whichever deals more damage; 20% poison
- Smack Down — hit airborne targets; give them Landed status (grounded)
- Snore — only usable while asleep; 30% flinch
- Sparkling Aria — deal damage; cure any burn on targets hit
- Spite — remove 4 PP from the target's most recently used move
- Spit Up — power 100/200/300 based on Stockpile level; fails without Stockpile status
- Steel Beam — 140-power; user takes 1/2 max HP recoil
- Stuff Cheeks — eat held Berry; +2 Defense
- Substitute — lose 1/4 max HP to create a substitute that absorbs damage
- Sucker Punch — priority move; fails if target didn't choose a damaging move this turn
- Super Fang — deal damage equal to 1/2 of target's current HP (min 1)
- Swallow — heal based on Stockpile level (1/4 / 1/2 / full HP); fails without Stockpile
- Tearful Look — −1 Atk and Sp. Atk; ignores evasion; hits through Protect
- Transform — become an exact copy of the target (all stats except HP)
- Upper Hand — flinch the target; fails if target isn't about to use a priority move
- Venoshock — 65-power; 2× vs. poisoned target
- Wish — restore 1/2 of user's max HP to the Pokémon in that slot next turn
