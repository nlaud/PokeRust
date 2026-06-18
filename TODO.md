# TODO

Entries are grouped into session-sized batches. Each group shares a common implementation
hook and should be researchable, plannable, and implementable in a single focused session.

---
## Abilities & Moves audit (2026-06-18)

Results of auditing a requested list of abilities/moves. Items verified **fully
implemented** were dropped from this list (abilities: Poison Point, Speed Boost,
Huge Power, Tough Claws, Synchronize, Telepathy, Shed Skin, Pickpocket; moves:
Leaf Blade / Blaze Kick high-crit ratio, Foul Play, Trick, Switcheroo, Assurance,
Infestation, Psych Up, Last Respects, Seismic Toss / Night Shade). Everything
below is **not implemented** or **only partially implemented** — research on
Bulbapedia before starting each batch.

### Contact-punishment abilities (hook: `simulator_helpers.rs` contact-punish match ~7860-7917, alongside Poison Point/Static)
- **Effect Spore** — NOT IMPLEMENTED (enum `ability.rs:65`, zero effect code). On contact: 9% poison / 10% paralysis / 11% sleep, independent roll per multi-hit strike. Immune: Grass types, Overcoat, Safety Goggles. Must not fire if the holder was KO'd by the hit.
- **Poison Touch** — NOT IMPLEMENTED (enum `ability.rs:186`, zero effect code). Holder's contact moves gain 30% chance to poison the target; independent roll per strike; blocked by Immunity / Shield Dust; applies independently of the move's own secondary. (Note: this is an *attacker-side* on-contact effect, mirror of the defender-side punish hooks.)

### Damage-modifier abilities (hook: damage calc in `simulator_helpers.rs`, near existing ability multipliers ~1149-1209 and `defender_type_reduction_mult` ~1390-1411)
- **Fluffy** — NOT IMPLEMENTED (only listed as Mold-Breaker-ignorable at `simulator_helpers.rs:3289`; no damage modifier). ×0.5 from contact moves, ×2 from Fire moves (Fire+contact = neutral). Long Reach negates the contact halving but Fire still doubles.
- **Reckless** — PARTIAL (`simulator_helpers.rs:1149-1153`). Two bugs vs Bulbapedia: (1) Struggle is wrongly boosted — condition includes `struggle_recoil` and Struggle is flagged `struggleRecoil`; Struggle must be excluded. (2) Crash-damage moves (Jump Kick, High Jump Kick) are NOT boosted — they use `has_crash_damage` with no `recoil_fraction`, so they fall through; they should get ×1.2. `has_crash_damage` exists at `dex_data.rs:425` but isn't referenced in the Reckless check.
- **Sheer Force** — PARTIAL (core damage ×5325/4096 and secondary suppression work, ~1207, ~8386). Latent gaps: should also suppress post-hit on-hit ability/item effects (Color Change, Pickpocket-on-being-hit, etc.) and Life Orb recoil — currently moot only because Life Orb recoil itself is unimplemented (`simulator_helpers.rs:7728`). Revisit when those features land.

### Status-immunity abilities
- **Good as Gold** — NOT IMPLEMENTED (variant absent from `ability.rs` entirely — needs enum regen per the code-gen note in CLAUDE.md). Grants immunity to other Pokémon's status moves (including ally status like Helping Hand) while still allowing self-targeted status moves; does NOT block whole-side moves (Stealth Rock) or all-Pokémon moves (Haze); makes Curse/Memento etc. fail without paying their self-costs.
- **Vital Spirit** — PARTIAL (sleep prevention works, `simulator_helpers.rs:5498`). Missing: (1) Yawn drowsy volatile should be *prevented from attaching* — the Yawn guard at ~6527-6533 only checks Sweet Veil, not Vital Spirit/Insomnia (net outcome currently still correct because the eventual sleep is blocked, but the volatile shouldn't apply). (2) Rest should fail when used by the holder. (3) No wake/cure handling if the ability is gained mid-battle while already asleep.

### Variable-base-power moves (hook: dynamic-BP match block `simulator_helpers.rs` ~963-1090, alongside Hex/Assurance)
- **Barb Barrage** — PARTIAL / signature mechanic missing (enum `pokemon_move.rs:50`; 50% poison secondary is data-driven and works). Missing: **double power vs targets with `psn`/`tox`** — needs a `BarbBarrage` arm doubling BP when `target.status` is poison/toxic (mirror the `Hex`/`InfernalParade` arm at ~1034).
- **Rage Fist** — NOT IMPLEMENTED (enum `pokemon_move.rs:662`; uses flat BP 50). Power = `50 + 50 × times_hit` (cap 350). **Blocked on infrastructure:** `PokemonState` has no persistent hit counter (only per-turn, end-of-turn-cleared damage flags). Need a `times_attacked` field incremented when hit by a damaging move, and (Champions rule) reset on switch-out/faint.

### Charging / two-turn moves (hook: `simulator.rs` charge handling ~291-380)
- **Skull Bash** — PARTIAL. Two-turn charge works via the generic `charge` flag, but the **+1 Defense on the charge turn** is missing — the charge-turn stat-boost special-case at `simulator.rs:343-351` only covers ElectroShot/MeteorBeam (+1 SpA); add a Skull Bash → +1 Def arm. Also: **Power Herb** (`item.rs:342`) has no logic anywhere, so no charge move executes in one turn via Power Herb (separate, broader gap).

### Locking / multi-turn moves
- **Rollout** — NOT IMPLEMENTED (enum `pokemon_move.rs:697`; fires once at static BP). Needs: 5-turn lock, power doubling each consecutive hit (30→60→120→240→480), reset on miss/interrupt/switch/Protect, and Defense Curl ×2 on the whole sequence. The `LockedMove` rampage system (`simulator.rs:5248-5250`) currently covers only Thrash/Outrage/Petal Dance/Raging Fury.
- **Focus Punch** — NOT IMPLEMENTED signature mechanic (enum `pokemon_move.rs:281`; priority −3 is data-driven and works). Missing: the "focusing" pre-move volatile and **fail-if-hit-by-a-damaging-move-before-attacking** check. Gen V+ nuances: status moves / Substitute hits / OHKO moves don't break focus; PP is still consumed on lost focus.
- **Uproar** — PARTIAL (self-lock, field-wide sleep prevention, and waking sleepers all work, `simulator_helpers.rs:5385/5454/8600`). Missing edge case: a holder hit by **Throat Chop** should have its Uproar end (calm down) at end of turn — Throat Chop currently only blocks selecting new sound moves (`simulator.rs:1188`).

### Move effects with no current handling
- **Curse** — NOT IMPLEMENTED (enum `pokemon_move.rs:156`; `VolatileStatus::Curse` exists only as a Baton-Pass-able tag, `simulator.rs:5999`). Ghost users: lose ½ max HP, apply Curse volatile that drains ¼ max HP per end of turn (Baton-Passable, ends on switch). Non-Ghost users: +1 Atk, +1 Def, −1 Spe. Behavior keys off the user's *current* type.
- **Topsy-Turvy** — NOT IMPLEMENTED (enum `pokemon_move.rs:888`). Inverts all of the target's stat stages (×−1); fails if all stages are 0; bypasses accuracy; does NOT trigger Defiant/Competitive and ignores Contrary/Simple/Clear Body/Mist.
- **Imprison** — NOT IMPLEMENTED (enum `dex_data.rs:185`; only no-op comments exist). No enforcement: opponents sharing a move with the Imprison user should be unable to select/use it. The move-restriction block at `simulator.rs:1185-1194` only handles Taunt/Throat Chop/Disable; needs the Imprison effect applied/persisted and checked there.
- **Dream Eater** — PARTIAL (50% drain heal works, `simulator.rs:5131`; Heal Block gating works). Missing: (1) **sleep-only restriction** — must fail unless the target is asleep or has Comatose (currently hits/heals off any target). (2) **Liquid Ooze reversal** on the generic drain path (`simulator.rs:5132`) — Liquid Ooze is only special-cased for Strength Sap / Leech Seed, so all `drain:` moves wrongly heal vs Liquid Ooze.

### Other / cross-cutting
- **Make It Rain** — PARTIAL (Steel/Special/120 BP/spread are data-driven). Verify Champions values: data has self `spa: -1` and `accuracy: 100` (Scarlet/Violet); audit research indicates Champions uses **−2 SpA** (and possibly 95% accuracy). Confirm on Bulbapedia and correct the data/source if so. Coin scatter has no battle effect (ignore).
- **Lucky Chant + always-crit / high-crit** — `SideCondition::LuckyChant` (`dex_data.rs:264`) is never checked by the crit system. It should prevent crits, including guaranteed-crit moves (Storm Throw, Frost Breath, Surging Strikes, Wicked Blow, Zippy Zap, Flower Trick) — none of which `crit_is_prevented`/`crit_is_guaranteed` currently consult. Niche but a real gap.

---
## Saved for later (Information only)
- Illusion — enter disguised as the last party member
- Anticipation — signal if opponents have SE or OHKO moves (message-only; no battle-state change in a full-information sim)
- Frisk — reveal an opponent's held item (message-only; no battle-state change in a full-information sim)

## Season 2 New Stuff
### Pokemon
Vileplume	Grass/Poison
Qwilfish	Water/Poison
Sceptile	Grass
Blaziken	Fire/Fighting
Swampert	Water/Ground
Mawile	Steel/Fairy
Metagross	Steel/Psychic
Staraptor	Normal/Flying
Musharna	Psychic
Scolipede	Bug/Poison
Scrafty	Dark/Fighting
Eelektross	Electric
Pyroar	Fire/Normal
Malamar	Dark/Psychic
Barbaracle	Rock/Water
Dragalge	Poison/Dragon
Grimmsnarl	Dark/Fairy
Falinks	Fighting
Overqwil	Dark/Poison
Houndstone	Ghost
Annihilape	Fighting/Ghost
Gholdengo	Steel/Ghost

## Refactors
- Recheck all tests, suggest new tests, verify mechanics.
