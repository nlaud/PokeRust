# `information/` — Fog-of-War Inference Engine

A Pokémon battle is played under partial information: each side sees the moves,
switches, damage, and status changes that happen on the field, but not the
opponent's EVs, IVs, nature, held item, or (until revealed) ability or exact
species form. This module turns the stream of *player-visible* events from a
battle into the tightest possible bounds on everything hidden about the
opponent's team — without ever guessing. Every bound it produces is backed by a
proof that no possible held item/EV spread/nature/etc. that could have produced
the observed events has been excluded.

It has two jobs, split across four files:

| File | Job |
|---|---|
| `information.rs` | Defines `InformationEvent` / `EventKind` — the vocabulary of everything a player can observe. |
| `unknowns.rs` | Defines `UnknownBattleState` / `UnknownPokemonState` — the fog-of-war mirror of the simulator's concrete state, plus the `Unknown<T>` lattice and the `Statement` constraint language. |
| `inference.rs` (+ `inference/bcp.rs`) | The six-pass engine that walks events and tightens the fog-of-war state. Entry point: `apply_information`. |
| `materialize.rs` | Turns a hypothesis (an `UnknownPokemonState` plus a candidate stat/item/ability choice) back into a concrete `PokemonState`/`BattleState` so the real damage-calculation code can be reused as an oracle. |

The rest of this document works through all four in depth. A companion file,
`AUDIT.md`, is the running log of soundness bugs found and fixed in this engine
(referenced throughout as "S17", "S23", etc.) — read it alongside this document
for the failure modes that motivated specific design choices.

---

## Part 1 — What a Player Sees: `InformationEvent`

### The nested tree model

Every observable occurrence is an `InformationEvent`:

```rust
pub struct InformationEvent {
    pub kind: EventKind,
    pub reactions: Vec<InformationEvent>,
}
```

This module differs from Showdown's flat SIM-PROTOCOL stream in one deliberate
way: **child events are nested inside the event that caused them**, in the
`reactions` field, rather than emitted as a flat sequence with a separate cause
tag. The parent always supplies the cause, so no event needs an "effect
source" field, and the inference engine can read an item/ability/secondary
effect in context without having to scan backwards through earlier entries.

A Life Orb Drain Punch that crits, gets resisted by a Sitrus Berry, and drains
back HP looks like:

```
MoveUsed { user: P1[0], move: DrainPunch, targets: [P2[0]] }
  ├── Crit { target: P2[0] }
  ├── DamageDealt { target: P2[0], new_hp: Percent(38) }          ← pre-berry HP
  ├── Healed { target: P2[0], new_hp: Percent(56) }               ← Sitrus Berry heal
  ├── ItemLost { slot: P2[0], item: SitrusBerry, consumed: true }
  ├── Healed { target: P1[0], new_hp: Number(185) }               ← drain
  └── DamageDealt { target: P1[0], new_hp: Number(162) }          ← Life Orb recoil
```

A pinch/HP berry (Oran, Sitrus, Figy, …) firing mid-hit is always its own
`Healed` reaction reporting the **post-berry** HP, immediately after a
`DamageDealt` reporting the **pre-berry** HP — the two are never folded into
one combined `DamageDealt`. Folding them would understate the damage actually
dealt, and Pass 3's damage→stat inversion reads that delta directly: an
understated delta can silently exclude the attacker's true offensive stat from
the feasible range (damage is monotone in the stat, so a smaller-than-true
delta simply doesn't fall in the roll range the true stat produces). The
berry's `ItemLost` is its own sibling event, emitted later by a whole-move
item-snapshot diff rather than nested under the specific `Healed` it explains —
this matches what a real player actually sees, in order.

A damage-*reducing* item (a type-resist berry like Occa Berry) works
differently: it produces no HP-change event of its own at all, because its
effect is baked directly into the single damage roll that produced
`DamageDealt`.

The inference engine walks this tree depth-first, so every event nested under
a `MoveUsed` automatically carries that move's context (user, move, targets)
without needing to look anything up.

### Ordering is the caller's job

`InformationEvent` says nothing about turn order or actor speed. Assembling
the `Vec<InformationEvent>` for a turn in priority/speed order is the
responsibility of whatever code drives the simulator loop. Because reactions
travel nested inside their cause, this ordering choice doesn't affect
correctness at any deeper level — Pass 4 (speed inference) is the one pass
that specifically depends on top-level event order, and it says so explicitly.

### `PokemonHP`: HP typed by visibility

```rust
pub enum PokemonHP {
    Number(u16),   // your own Pokémon — exact HP
    Percent(u8),   // an opponent — the displayed percentage, 0–100
}
```

This mirrors what a real player's screen shows. The inference engine exploits
this asymmetry directly: when your Pokémon takes damage, the exact HP delta is
available (Pass 3 "Direction B"); when you deal damage to an opponent, only
two rounded percentages are available and the true delta must be recovered
from the display-rounding math (Pass 3 "Direction A").

### The `EventKind` catalogue

`EventKind` is grouped, mirroring Showdown's protocol categories:

| Category | Variants |
|---|---|
| Major actions | `MoveUsed`, `Switch`, `SimultaneousSwitch`, `Faint`, `EndOfTurn`, `Cant`, `ChargingMove`, `MustRecharge`, `SingleMoveOrTurn` |
| Form/identity changes | `MegaEvolution`, `Terastallization`, `FormeChange`, `TypeChanged`, `Transformed`, `IllusionEnded` |
| HP changes | `DamageDealt`, `Healed`, `SetHp` |
| Hit qualifiers | `Crit`, `Immune`, `Missed`, `MoveFailed`, `Blocked`, `HitCount` |
| Status | `StatusInflicted`, `StatusCured`, `TeamStatusCured` |
| Stat stages | `BoostChanged`, `BoostsCleared`, `BoostsInverted`, `BoostsSwapped`, `BoostsCopied` |
| Field | `WeatherChanged`, `TerrainChanged`, `PseudoWeatherStart`/`End` |
| Side/slot | `SideConditionStart`/`End`, `SlotConditionStart`/`End` |
| Volatiles | `VolatileStart`, `VolatileEnd`, `PerishCount` |
| Items | `ItemRevealed`, `ItemGained`, `ItemLost` |
| Abilities | `AbilityRevealed`, `AnticipationShudder` |

Each variant's doc comment in `information.rs` records exactly which real
mechanics route through it and any per-variant subtlety (e.g. `MoveFailed`'s
cause is always conveyed by a nested event rather than a tag; a `Transformed`
event is nested under the `Switch`/`MoveUsed` that caused it and records both
the transformer's slot and the slot it copied). Two mechanics are deliberately
*not* discrete events because they're better modelled as state:

- **Ability suppression** (Gastro Acid, Neutralizing Gas) is tracked as a
  volatile / field-wide scan, mirroring `pokemon_ability_is_suppressed` in
  `simulator::helpers`, rather than an event that would need to be un-done later.
- **Priority/speed ordering** — see above.

---

## Part 2 — How Partial Knowledge Is Stored: `UnknownBattleState`

### The `Unknown<T>` lattice

Every hidden attribute is one of three states:

```rust
pub enum Unknown<T> {
    Known(T),          // definitively identified
    Not(Vec<T>),        // could be anything except these excluded values
    Possibly(Vec<T>),  // must be one of these candidates
}
```

`Known` is the narrowest possible state; `Not(vec![])` is the widest (nothing
excluded yet). Information only ever flows one direction — towards `Known` —
and the engine's central invariant is that **the true value is always
contained within whatever `Unknown` currently describes it**. An update that
would risk excluding the true value is never performed; the engine unions
possibilities instead and lets a later, more specific observation narrow
things down.

`Possibly` is used for ordinary multi-candidate uncertainty (e.g. a fresh
opponent sighting's ability set, `Possibly(dex[species].abilities)`). Illusion
(Zoroark) is deliberately **not** modelled this way — `possible_species` stays
pinned to a single `Known` even for a mon that might be a disguised Zoroark; see
"Illusion: the parallel-hypothesis model" below for how that ambiguity is
tracked instead.

### Per-Pokémon fog: `UnknownPokemonState`

Every attribute of an opponent's Pokémon that isn't directly visible is either
an `Unknown<T>` or a min/max range:

```
possible_species, possible_types, item, possible_natures,
possible_abilities, possible_original_abilities, possible_weight_hg,
possible_tera_type, possible_genders, mega_species, mega_ability : Unknown<T>

min_evs / max_evs, min_ivs / max_ivs           : [u8; 6]   per-stat bounds
min_stats / max_stats                        : [u16; 6]  derived stat bounds
min_pre_nature_stat / max_pre_nature_stat  : [u16; 6]  pre-nature BSV bounds
```

`min_pre_nature_stat`/`max_pre_nature_stat` store bounds on the **pre-nature
base stat value** — `calc_stat(base, iv, ev, level, 1.0)`, i.e. the stat before
the ×0.9/1.0/1.1 nature multiplier. This intermediate quantity matters because
Pass 3 (damage inversion) can pin down a stat's value *before* knowing the
Pokémon's nature; Pass 5 then combines these bounds with the possible natures
to back-solve EV/IV ranges. Everything else the fog-of-war side of the engine
needs to track — items consumed/lost/gained, per-turn flags read by Counter,
Metal Burst, Assurance, Avalanche, Rage Fist, Cud Chew's delayed re-eat, Choice
lock provenance, and so on — mirrors the equivalent field on the simulator's
concrete `PokemonState` field-for-field, so that `UnknownPokemonState` can walk
in lockstep with the real battle regardless of how much is currently unknown.

Two ability fields exist because the *innate* ability and the *currently
active* ability can diverge mid-battle:

- `possible_original_abilities` — the Pokémon's innate ability (from its
  species' ability slots). Changes only on Mega Evolution or a permanent forme
  change.
- `possible_abilities` — the ability actually in effect right now. Diverges
  from the original after Trace, Mummy, Skill Swap, Entrainment, etc., and
  resets back to `possible_original_abilities` on switch-out.

A freshly-seen opponent Pokémon is built by `UnknownPokemonState::from_opponent_species`:
stat bounds are the theoretical worst case (0 IV / 0 EV / hindering nature) and
best case (31 IV / 252 EV / boosting nature) computed independently per stat
(natures only touch one stat each way, so a single assumed nature would be
unsound), and the ability set becomes `Possibly(dex[species].abilities)` when
dex data exists. Your own Pokémon is built by `from_known_pokemon`, where every
`Unknown` collapses to `Known` and every range collapses to a point, from the
start.

### The CNF predicate store

Not every observation can be committed straight to a field — many can only be
expressed as "at least one of these is true." `UnknownBattleState::predicates`
holds these as conjunctive normal form:

```
predicates: Vec<Vec<Statement>>
          = (stmt ∨ stmt ∨ …) ∧ (stmt ∨ stmt ∨ …) ∧ …
```

the outer `Vec` is an AND of clauses, each inner `Vec<Statement>` is an OR of
literals. For example, observing a 100%-accurate move miss with no other
explanation emits the clause `[HasItem(BrightPowder), HasItem(LaxIncense)]` —
"this Pokémon holds at least one of these two items" — without committing to
either until further evidence (or Boolean Constraint Propagation) narrows it
to one.

`Statement` variants:

| Statement | Meaning |
|---|---|
| `HasItem { mon_idx, item }` | Holds this item |
| `HasAbility { mon_idx, ability }` | Has this ability |
| `HasMove { mon_idx, pokemon_move }` | Knows this move (used by learnset narrowing plumbing) |
| `HasStatus { mon_idx, status }` | Afflicted with this status |
| `NatureBoostsStat` / `NatureNerfsStat { mon_idx, stat }` | Nature gives ±10% to this stat |
| `EVIVStatGE` / `EVIVStatLE { mon_idx, stat, value }` | Pre-nature BSV is ≥/≤ `value` |
| `SpeedComparison { fast_idx, slow_idx, fast_mult, slow_mult }` | `spe(fast)×fast_mult ≥ spe(slow)×slow_mult` |
| `KnowsThreateningMove { mon_idx, defender_types }` | Knows a move super-effective (or OHKO) against these types — from Anticipation |
| `WeatherTurns` / `TerrainTurns` / `SideConditionTurns { turns }` | A field timer equals exactly this value |

`SpeedComparison` and `KnowsThreateningMove` are *relational* constraints —
they compare two Pokémon or relate a Pokémon to a hypothetical move rather
than pinning one field — so BCP never "forces" them into a concrete value the
way it does `HasItem`/`HasAbility`/etc.; it only ever satisfies or prunes the
clause they sit in. `propagate_speed_comparisons` is the code that turns a
*unit* `SpeedComparison` clause into an actual tightening of `min_stats[5]`/
`max_stats[5]`.

### `mon_idx`: a flat index across every roster

Every `Statement` and setter field refers to a Pokémon by a single `usize`,
`mon_idx`, resolved against this fixed order:

```
[p1_active…, p2_active…,
 p1_known_back…, p1_possible_back…,
 p2_known_back…, p2_possible_back…]
```

Note that **both sides' actives come first**, ahead of either side's bench.
This is deliberate (see AUDIT.md "S1"): a naive per-side-contiguous layout
(`[p1_active, p1_back, p2_active, p2_back]`) meant that any switch on P1 — which
grows or shrinks P1's bench `Vec`s via push/remove — shifted every index after
it, silently retargeting P2's *persisted* `Statement`s (which survive across
turns) onto the wrong physical Pokémon. With both active segments fixed at the
front, only `p1_active_mons.len()` and `p2_active_mons.len()` matter for any
active mon's index, and both are permanently stable after the initial lead
bootstrap (`pass1_switch` always overwrites an active slot in place, never
push/removes). Only bench indices are still unstable, but nothing persists a
`Statement` referencing a bench index across events, so that's sound. The
trade-off: a side's full roster is no longer one contiguous range, so helpers
that need "everything on side X" (`teammate_indices`, `TeamStatusCured`,
`mon_is_p2`) check each segment explicitly instead of one `[start, end)` span.
Use `mon_idx_for_active_slot`, `get_mon_by_idx`, and `get_mon_mut_by_idx` to
resolve indices rather than computing offsets by hand.

---

## Part 3 — The Six-Pass Inference Pipeline

Entry point: `apply_information(state, events, dex, config)`. Every call runs
all six passes, in order, over the full event list for one turn:

```
apply_information_battle
  ├── Pass 4  speed ordering → Spe bounds          (run FIRST, to pre-warm bounds)
  ├── propagate_speed_comparisons()                 (immediate fixpoint)
  ├── [event walk: Passes 1–3, depth-first per event]
  ├── Pass 5  back-solve EV / IV / nature
  └── Pass 6  BCP to fixpoint
         └── propagate_speed_comparisons()          (also runs inside the BCP loop)
```

Pass 4 runs first, before the main walk, specifically so that Pass 3's
speed-dependent base-power moves (Gyro Ball, Electro Ball) see already-tightened
speed bounds. After the main BCP loop, if BCP just forced a priority-lifting
ability (Prankster, Gale Wings, Triage) to `Known`, Pass 4 and
`propagate_speed_comparisons` are re-run once more, since that newly-resolved
ability can retroactively remove an escape disjunct from an earlier
speed-order clause.

### Pass 1 — Structural / Direct Facts

**Where:** `process_battle_event` / `pass1_apply_event`, the depth-first walk
of every `InformationEvent` and its nested reactions.

This pass updates fields directly from what an event explicitly states:
`AbilityRevealed`/`ItemRevealed` narrow their respective `Unknown` to `Known`;
`ItemLost` sets `consumed_item` or `removed_item` depending on `consumed`;
status/boost/field events update state directly; `Switch`/`SimultaneousSwitch`
record species/level/HP/status and build a new `UnknownPokemonState` via
`from_opponent_species` for a never-before-seen Pokémon;
`Terastallization`/`MegaEvolution`/`FormeChange` update the corresponding
identity fields; `MoveUsed` fills `known_moves` via `reveal_move_on_mon` and
triggers learnset-based species narrowing (below). A contradiction — e.g. an
`ItemRevealed` for a Pokémon whose item is already `Known` as something else —
panics immediately via `inference_contradiction!`, since it means the observed
events are jointly impossible and something upstream is wrong.

One field is derived from an event's *context* rather than its contents:
`rest_sleep`. Rest puts its user to sleep for a deterministic 2 blocked turns
(1 with Early Bird) instead of the weighted random duration, and a bare
`StatusInflicted { Sleep }` cannot tell the two apart — so the `MoveUsed` arm
sets the flag when the move is Rest and one of its own reactions is a Sleep on
the user. The two halves are deliberately split so their order does not matter:
`MoveUsed` only ever *sets* the flag (and only for Sleep), while
`StatusInflicted`/`StatusCured`/`TeamStatusCured` only ever *clear* it (and
never for Sleep). `false` is the safe default — it under-claims, treating a
Rest sleep as an ordinary one, never the reverse.

Ability tracking has two extra wrinkles handled here:

- If a revealed ability is inside the current candidate set, it narrows to
  `Known` normally. If it's *outside* the set, that means a live ability
  change happened (Trace copying a foreign ability, Mummy, etc.) — only
  `possible_abilities` is overwritten; `possible_original_abilities` is
  untouched.
- A switch of a previously-seen Pokémon resets `possible_abilities` back to
  `possible_original_abilities` (Trace/Skill Swap don't persist across a
  switch), and Mega Evolution/forme changes recompute both fields from the new
  species' ability set.

**Slot re-binding (S18).** `mon_idx` identifies a *slot*, not a physical
Pokémon — the same index means a different Pokémon before and after a switch.
`purge_mon_scoped_knowledge` runs at the moment an occupant leaves a slot,
dropping every `Statement` referencing that slot's index and nulling matching
weather/terrain/screen-setter records, so a fresh occupant never silently
inherits a predicate that was actually about the Pokémon that just left.
Dropping constraints only ever widens the fog, so this is sound by
construction.

**Item epoch tracking (S19/S20).** A `HasItem` clause means "held item X *at
the time the clause was emitted*," not "holds item X right now." Every
item-change event (`ItemLost`, `ItemGained`, Trick/Switcheroo) resolves
outstanding item literals against the item the Pokémon was holding *before*
the change: a literal that was historically true satisfies and drops its
clause; one that was historically false is pruned from it. This keeps a
clause like `[HasItem(BrightPowder) ∨ HasItem(LaxIncense)]` from becoming an
impossible clause just because the mon later reveals a third, unrelated item
via Knock Off. `item_was_transferred` similarly gates Choice-lock exclusion
(`pass1_choice_exclusion`): using two different moves is normally proof of no
Choice item, but not if a Choice item arrived mid-stint via Trick — that's
consistent with "moved, was Tricked a Choice item, then legally moved again."

**Illusion and the bench.** Under the current parallel-hypothesis model (see
"Illusion: the parallel-hypothesis model" below), `possible_species` is always
pinned — Zoroark ambiguity lives entirely in a mon's own `possible_illusion_state`,
which rides along for free whenever `pass1_switch`/`bench_outgoing_mon` move a
whole `UnknownPokemonState` between active and bench. So switch-in matching is
always plain species-matching against the bench, with no special-casing needed
for a suspected disguise: whichever physical roster member's shown species
matches is exactly the one being pulled onto the field, hypothesis (if any)
attached. (This superseded an earlier design, S29, where a suspected disguise
widened `possible_species` itself to `Possibly([shown, zoroark])` and had to be
discarded rather than benched on an unresolved switch-out to avoid double-
counting a roster slot — the parallel-hypothesis model has no such gap, since
`possible_species` never widens and every non-fainted leaver benches uniformly.)

### Pass 2 — Item / Ability Presence and Absence from Behaviour

**Where:** helper functions named `pass2_*` / `pass_eot_*`, called from
`process_battle_event` after a `MoveUsed`'s reaction tree or an `EndOfTurn`
event is fully walked.

Reactive items/abilities are modelled with a **nested-reveal convention**: the
item or ability that *caused* a reaction is always emitted as its own
`ItemRevealed`/`AbilityRevealed` nested inside the triggering move event, never
as a bare, unattributed effect. That means Pass 1 already handles *presence* —
Pass 2 exists almost entirely to reason about **absence**, deducing what a
Pokémon does *not* have from an effect that didn't happen when it should have.
Representative clauses it emits (each gated on the escapes that would let the
event legitimately not fire):

- **Life Orb presence** — a `DamageDealt { target == user }` at the Life Orb
  recoil fraction adds `HasItem(LifeOrb)` (gated on the item being active:
  no Magic Room, no Klutz — S21).
- **Contact-reaction absence** — a contact move that hits and produces no
  Rocky Helmet/Rough Skin/Iron Barbs reaction excludes all three from the
  defender, unless the attacker has an escape (Long Reach, Magic Guard,
  Protective Pads).
- **Choice-item absence** — two different moves used in consecutive turns
  excludes Choice items (subject to the `item_was_transferred` guard above).
- **Bright Powder / Lax Incense** — a 100%-accurate move missing, with neither
  side's accuracy/evasion stages explaining it, emits the two-item disjunction.
- **Powder-move immunity** — a powder move blocked on a non-Grass target
  emits `[HasItem(SafetyGoggles) ∨ HasAbility(Overcoat)]`.
- **Guaranteed-status absence** (`pass2_guaranteed_status_absence`) — a
  100%-chance secondary status that should have landed but didn't emits the
  disjunction of every *undecided* preventer for that status (decidable ones —
  type immunity, existing status, Substitute, Safeguard, terrain — are ruled
  out first). Freeze is skipped entirely in harsh sunlight, where it's simply
  impossible regardless of ability.
- **Prankster immunity** (`pass2_prankster_immunity`) — a status move blocked
  by a Known Dark-type target is a unit clause forcing `Known(Prankster)` on
  the user immediately.
- **Ground-move immunity** (`pass2_ground_immune_clause`) — a Ground move
  whooshing past a non-Flying, non-Magnet-Rise/Telekinesis target emits
  `[HasItem(AirBalloon) ∨ HasAbility(Levitate) ∨ HasAbility(Eelevate) ∨ HasAbility(EarthEater)]`.

End-of-turn absence checks (`pass_eot_heal`, `pass_eot_sand_immunity`,
`pass_eot_self_status`) work the same way: unexplained EOT healing with every
decidable source (Aqua Ring, Ingrain, Grassy Terrain, Wish, Leech Seed) ruled
out emits `[HasItem(Leftovers)]` (or the Leftovers/Black Sludge pair for
Poison-types); a fresh self-inflicted Burn or Toxic at EOT with no prior status
is forced straight to `Known(FlameOrb)`/`Known(ToxicOrb)` since no other
mechanic causes that; and no Sandstorm chip on a non-Rock/Ground/Steel target
emits the full disjunction of every ability/item that can grant sand immunity,
skipping any literal already excluded.

### Pass 3 — Damage → Stat Bounds

**Where:** `pass3_damage_to_stats`, run per `MoveUsed` after its full reaction
tree (and thus every `DamageDealt`/`Crit`) has been walked.

This is the computationally heaviest pass, and the one with the most
subtlety. Rather than analytically inverting the damage formula — fragile,
given its ~20-odd flooring/rounding steps — it treats the simulator's own
damage function, `calculate_damage_outcomes_for_target_with_options`, as a
**forward oracle**: enumerate candidate stat values, run the real damage
calculation, and keep only the candidates consistent with what was observed.

Some moves carry no invertible stat signal at all and are skipped outright:
status moves, OHKO moves, fixed-damage moves (Seismic Toss), retaliation moves
(Counter/Mirror Coat/Metal Burst/Comeuppance), ambiguous-stat moves (Shell
Side Arm, Photon Geyser), and Beat Up.

**Direction B — the opponent hits you (exact HP).** The delta
`pre_hp - post_hp` is exact. The oracle binary-searches the attacker's
pre-nature BSV range (`find_feasible_bsv_range_b`) for the interval that can
produce that exact damage, exploiting the fact that damage is monotone in the
attacking stat — the feasible set for a fixed (item, ability) combination is
always a contiguous interval, so a binary search plus a short linear refinement
finds its endpoints in `O(log range)` oracle calls instead of scanning
one-by-one. Item/ability boosters (Choice Band, Life Orb, etc.) are each
scanned separately and their feasible ranges unioned (sound: wider), with
conditional CNF clauses emitted for cases BCP can resolve later.

**Direction A — you hit the opponent (percent HP).** Only two rounded display
percentages are available. `percent_bucket` inverts the display-rounding
formula (`hp_to_percent`) exactly for a candidate max HP, giving each
percentage its own raw-HP bucket; the true damage band is
`[pre_lo - post_hi, pre_hi - post_lo]` (S22 — treating the *delta* as a single
rounding, rather than each side independently, silently excludes real damage
values and with them the true stat, especially for high-HP defenders).
`achievable_defender_hp_values` enumerates exactly the HP values the
defender's species can actually reach under the stat-points lattice, rather
than a fixed stride, so no feasible BSV is lost to picking an unreachable HP
along the way. The defensive item/ability search (`defensive_damage_items`/
`defensive_damage_abilities`) enumerates every damage-reducing modifier the
oracle implements — completeness here is a **soundness invariant**, checked by
a dedicated cross-validation test (`test_sc_allowlist_completeness_cross_validation`)
that runs the oracle against every ability/item the simulator knows and asserts
anything that changes damage is on the list. A pruning step drops
provably-inert (item, ability) combinations for the specific move being
evaluated (e.g. a Water-resist berry against a Fire move) purely for
performance — never in a way that could drop a live possibility.

**Per-hit walk (S23).** Multi-hit moves are walked hit-by-hit rather than as
one aggregate: a `Crit` reaction sets a pending flag consumed only by the
*next* `DamageDealt` (a global "any hit critted" flag would wrongly apply the
crit multiplier to non-crit hits in the same sequence); `Healed`/`SetHp`
reactions move the running HP baseline without counting as a hit, so a
pinch-berry heal mid-sequence doesn't understate the next hit's damage; and
each hit is materialized at its own true pre-hit HP rather than the
already-advanced post-move HP that Pass 1 has by the time Pass 3 runs — this
matters because Multiscale/Shadow Shield/Tera Shell are full-HP-gated and
would otherwise read as inactive for exactly the hit that broke full HP. A
related fix (S24) snapshots the attacker's and targets' *entire* fog state at
`MoveUsed` time (`MoveContext`), since Pass 3 runs after Pass 1 has already
applied the whole reaction tree — without the snapshot, a resist berry
consumed by the very hit being analyzed, a self-boost secondary, or a
mid-move burn would all be read in their post-move (already-changed) form
instead of the pre-move form the actual damage roll saw.

**Analytic and speed-dependent moves.** The oracle materializes an empty
action queue, so its own "moved last" check for Analytic is always true — Pass
3 instead decides this from the real event stream (S28) via
`compute_analytic_last_movers`, which records the one slot that actually
committed a move last each turn segment (a `Switch` doesn't count; a flinch,
full paralysis, or a Cant still needs to be handled correctly, which the naive
"did the target already move?" heuristic got wrong). Gyro Ball/Electro Ball's
speed-dependent base power is evaluated at both ends of the unknown Pokémon's
speed range and both results are kept (sound over-approximation), benefiting
from Pass 4 already having tightened those bounds by the time Pass 3 runs.

### Pass 4 — Speed Ordering → Spe Bounds

**Where:** `pass4_speed_from_order`, over the top-level event list (not
recursive), run once before the event walk and once more after BCP settles.

Every consecutive pair of same-priority-bracket `MoveUsed` events reveals a
speed relationship. Effective priority folds in Grassy Glide's terrain bonus
and, once an ability is already `Known`, priority-shifting abilities
(Prankster, Gale Wings, Triage); when it's not yet known, those abilities
become escape disjuncts instead (see below). Trick Room swaps which side of
the comparison is "faster." Deterministic multipliers — boost stage,
Paralysis, Tailwind — are folded into the comparison using a shared integer
denominator; anything conditional on a *hidden* modifier (Choice Scarf, Iron
Ball, weather-boosted abilities, Unburden, Quick Feet) is never folded in,
since doing so would make the derived bound unsound until that modifier is
actually excluded — it becomes an escape disjunct on the emitted clause
instead:

```
[SpeedComparison{fast_idx, slow_idx, fast_mult, slow_mult}]
  ∨ HasItem(QuickClaw | ChoiceScarf) on fast_idx
  ∨ HasAbility(QuickDraw | Prankster* | GaleWings* | Triage* | SwiftSwim** | Chlorophyll** | SandRush** | SlushRush** | SurgeSurfer** | Unburden*** | QuickFeet****) on fast_idx
  ∨ HasAbility(Stall) on slow_idx
  ∨ HasItem(IronBall | LaggingTail | FullIncense) on slow_idx
```
*(gated respectively on move category/type/full-HP, weather/terrain, item-lost state, and existing status; already-excluded literals are omitted.)*

**Snapshot timing (S4).** The boost/paralysis/Tailwind values folded into
each mover's multiplier are taken from a running snapshot built by scanning
`top_events` in order — reflecting every earlier action's effect *this turn*
up to that mover's own `MoveUsed` — rather than read live from `state` at
Pass 4's single call time (which would miss e.g. an earlier Thunder Wave that
paralyzed this mon before it acted). Getting this wrong bakes a wrong
numeric multiplier into a `SpeedComparison`, which is a soundness risk (a
stale multiplier can manufacture a spurious bound), not just imprecision.
`BoostsSwapped`/`BoostsCopied` are deliberately not tracked by this
snapshot — which stat moved isn't recoverable from the event alone — a
narrow, documented gap that doesn't threaten soundness since the escape
disjuncts remain valid regardless.

`propagate_speed_comparisons` (invoked immediately after Pass 4 and again
every BCP iteration) is the only code that turns a `SpeedComparison` into an
actual Spe bound, and only once it sits alone in a **unit** clause (S17 — a
`SpeedComparison` still sharing its clause with a live escape disjunct is
conditional; the observed order might be a Quick Claw proc, not genuine
speed, and enforcing it anyway can force a mon's minimum speed above its own
species maximum):

```
fast_min × fast_mult ≥ slow_min × slow_mult  ⟹  fast.min_stats[5]  ≥ ceil(slow.min_stats[5] × slow_mult / fast_mult)
fast_max × fast_mult ≥ slow_max × slow_mult  ⟹  slow.max_stats[5]  ≤ floor(fast.max_stats[5] × fast_mult / slow_mult)
```

Those temporary bounds feed Pass 5, but the final scalar Speed range is widened
back to the independent BSV/nature marginal before returning. The relational
fact remains in the CNF predicate store; keeping both a hard scalar projection
and the relation over-constrains marginals when multiple builds satisfy the same
ordering.

**Doubles soundness envelope.** Damage inversion in doubles still has unmodeled
cross-target and temporal interactions. As a conservative final step, doubles
states discard clauses containing nature/EV/IV build literals and widen uncertain
opponent nature, EV, IV, and stat fields back to the species-level theoretical
envelope. Singles retains the full Pass 3/Pass 5 narrowing. Item, ability, move,
identity, field, and relational speed inference remain active in doubles. This is
an intentional precision tradeoff: an admitted build may be too broad, but it
must not exclude the true build.

### Pass 5 — Back-Solve EV / IV / Nature

**Where:** `pass5_back_solve`, run per Pokémon with a `Known` species, after
the event walk.

Given the `min_stats`/`max_stats` (and, for non-HP stats, `min_pre_nature_stat`/
`max_pre_nature_stat`) tightened by the earlier passes, this pass inverts the
stat formula to constrain EV, IV, and nature directly.

**HP** has no nature term: enumerate `iv ∈ [min_ivs[0], max_ivs[0]]` (or just
`{31}` under `force_max_ivs`) and `ev` over the legal lattice, keep pairs whose
`calc_hp` falls inside `[min_stats[0], max_stats[0]]`, and tighten `min_evs[0]`/
`max_evs[0]` to the surviving range.

**Non-HP stats** iterate over every still-possible nature. For each nature's
modifier `m ∈ {0.9, 1.0, 1.1}`, a `(iv, ev)` pair survives if both the final
stat (`floor(BSV × m)`) falls in `[min_stats[i], max_stats[i]]` *and* the BSV
itself falls in `[min_pre_nature_stat[i], max_pre_nature_stat[i]]`. A nature
that has no surviving pair for any stat it touches is excluded outright. The
global EV range per stat is the union across all surviving natures.

**Cross-stat EV-total tightening.** When `config.ev_total_cap` is set
(510 by default for competitive play), each stat's ceiling is additionally
capped at `cap − Σ(other stats' floors)` — this only ever lowers a ceiling,
never raises a floor, so it's always sound, and gets tighter as more stats
develop high confirmed minimums.

**The stat-points EV lattice** (`EV_LATTICE`, 33 values: `0, 4, 12, 20, …, 252`)
mirrors `scale_evs_for_stat_points`'s `ev = max(0, 8×points − 4)` mapping, used
whenever `config.use_stat_points` is set to match the simulator's
`--stat-points` flag.

### Pass 6 — Boolean Constraint Propagation

**Where:** `inference::bcp::run_bcp`, iterated to a fixpoint.

Each pass over `state.predicates` applies three rules per clause: drop any
literal already known false (an empty clause means the observed events are
jointly impossible — panic); drop the whole clause if any literal is already
known true (already satisfied); and if exactly one literal survives and it
isn't a relational constraint (`SpeedComparison`, `KnowsThreateningMove`),
force it into the state via `force_literal` — `HasItem`/`HasAbility` narrow the
corresponding `Unknown` to `Known`, `HasMove` reveals a move slot,
`EVIVStatGE`/`LE` raise/lower a pre-nature bound, and so on. Relational
constraints stay in the predicate store permanently; `propagate_speed_comparisons`
re-derives concrete Spe bounds from them at the end of every iteration. Because
forcing or excluding a literal can turn some *other* clause into a fresh unit
clause, the whole loop runs to a fixpoint rather than a single pass.

---

## Illusion: the parallel-hypothesis model

Zoroark's Illusion ability lets a switch-in *look like* any other non-fainted
party member. This is the one place the engine tracks physical-identity
ambiguity, and it does so without ever widening `possible_species` into a
`Possibly` disjunction (an earlier design did this — see the historical note
under Pass 1's "Illusion and the bench" above — and was superseded because a
widened species can't be re-matched by species on the next switch-in without
extra bookkeeping).

**The model:** every mon that *could* secretly be the side's disguised Illusion
forme carries a second, full `UnknownPokemonState` in its own
`possible_illusion_state: Option<Box<UnknownPokemonState>>` field — "the
restrictions on this physical mon IF it is actually Zoroark, disguised as
whatever this mon's own shown species is." `possible_species` itself never
moves; the ambiguity lives entirely in this parallel sub-state.

**Seeding.** `seed_illusion_hypotheses` runs once, at the team-preview→battle
transition (`into_battle_state`) — the only moment a side's whole roster is
known and freshly built. It counts the side's real Illusion-capable roster
members into `p{side}_unresolved_zoroark_count` (`UnknownBattleState`; almost
always 0 or 1 — Species Clause permits at most one) and, if `> 0`, attaches a
hypothesis (`seed_illusion_hypothesis_for`) to every *other* roster entry from
that Illusion forme's own baseline: identity fields (species, types, ability,
moves, item, nature, stat bounds, …) come from the baseline, while every
physically-observable field (HP, status, boosts, volatiles, `times_hit`, …)
comes from the host slot — both describe the same physical mon, just under a
different identity hypothesis. From then on, ordinary bench/active bookkeeping
(`pass1_switch`, `bench_outgoing_mon`) moves the *whole* `UnknownPokemonState`
between active and bench, so the nested hypothesis rides along for free; no
switch handler needs Illusion-specific logic.

**Mirroring every pass.** `apply_with_illusion_mirroring` (`inference.rs`) is
the generic wrapper: given a fallible per-mon operation `f` (the same one
already used for the primary — e.g. `check_move_legal_for_species` for a
learnset-legality check, or a Pass 3 stat-bound tightening), it replays `f`
against the hypothesis first, then the primary, and resolves the four-way
outcome:

| Hypothesis under `f` | Primary under `f` | Result |
|---|---|---|
| OK | OK | `Unchanged` — both survive, kept side by side |
| contradicts | OK | `HypothesisRejected` — not Zoroark; hypothesis dropped |
| OK | contradicts | `Promoted` — IS Zoroark; `promote_illusion_to_primary` replaces the mon's fields wholesale |
| contradicts | contradicts | genuine impossibility — panics as usual |

`mirror_infallible_on_illusion` is the non-fallible counterpart, used for pure
state transitions (e.g. clearing per-turn flags on switch-out) that can't
contradict and so don't need the panic-catching machinery. This wiring covers
Pass 1 move/item reveals, Pass 3 damage→stat tightening (both directions), and
Pass 5 back-solve — a move or a damage roll that's impossible for the shown
species but possible for Zoroark silently promotes the hypothesis.

**Resolution.** Whenever a `Promoted` outcome fires, or an `IllusionEnded`
event arrives (a direct-damage disguise break, or an ability
suppression/change), or the real Illusion forme itself switches in undisguised
(only it can ever be shown *as* its own species), the caller follows up with
`resolve_zoroark_globally`: decrement `p{side}_unresolved_zoroark_count`, and
at 0, drop every remaining `possible_illusion_state` on the side — the side's
one Illusion forme (Species Clause) is now fully accounted for, so no other
mon needs to keep carrying a hypothesis.

Every `Promoted` site — Pass 1 move/item reveals, Pass 3's stat-tightening
backstop surfacing through Pass 5, and the dedicated `IllusionEnded` handler —
also follows up with `finish_illusion_promotion_restore`: the DISCARDED
shown-species identity (captured by the caller before promotion overwrote it)
was never really on the field and must be restored to `possible_back`, via
`restore_discarded_primary_to_bench`. This matters because promotion routinely
happens via move-legality (or a stat/damage contradiction) *before* the
disguise ever visibly breaks in-game — a status move revealing an
illegal-for-the-shown-species moveslot never deals damage, so `IllusionEnded`
may not fire until much later, if ever. An end-to-end server run against a
real open-sheet team surfaced exactly this gap: restoring the decoy only from
the `IllusionEnded` handler meant a promotion that happened earlier (say, from
Nasty Plot on a disguised Zoroark) left the decoy specie's `possible_species`
already overwritten by the time (if ever) `IllusionEnded` arrived — so that
handler's own "what was discarded" capture read the *already-promoted* Zoroark
identity, not the decoy, and skipped the restore too. Calling
`finish_illusion_promotion_restore` at every promotion site, right when it
happens, closes that gap regardless of which mechanism triggered it.

`restore_discarded_primary_to_bench` itself prefers a clone of the decoy
species' pristine team-preview snapshot (`p{side}_roster_templates`, captured
once at `into_battle_state`) over rebuilding species-only, so an
open-team-sheet mon's already-known item/moves/ability/nature survive the
reveal instead of regressing to "no information." The same roster-template
preference applies to `pass1_switch`'s defensive "species not found on the
bench" fallback. The `IllusionEnded` handler additionally purges CNF
predicates derived from the now-stale disguise species' base stats/movepool.

**Resolution is not permanent — re-disguise on re-entry.** Illusion
re-activates on *every* switch-in with no "already revealed" suppression
(`simulator::helpers::compute_illusion_disguise` always recomputes the
disguise from the current back-mons list). So a Zoroark that was positively
located earlier in the battle, then switches back out, can re-enter later
disguised as a *different* decoy than the one it was resolved from. Treating
`resolve_zoroark_globally`'s decrement as one-way left the belief with no way
to recover: once `p{side}_unresolved_zoroark_count` hit 0, every hypothesis
side-wide was gone for good, so the re-disguised mon's next signature-move
reveal had no hypothesis to promote into and hard-panicked in
`check_move_legal_for_species` instead.

`rearm_zoroark_on_side` (`inference.rs`) closes this gap: `bench_outgoing_mon`
calls it whenever the mon being benched has a `Known`, Illusion-capable
`possible_species` — i.e. a previously-resolved Zoroark heading back to the
bench. It bumps the count back up (capped at the side's true Illusion-roster
size) and re-seeds `possible_illusion_state` on every other eligible mon from
the just-benched entry (which already carries everything learned about this
physical Zoroark this battle, more precise than falling back to the
team-preview template). Ordering makes this transparent to `pass1_switch`:
every switch event benches ALL outgoing mons before pulling ANY incoming one,
so by the time the new decoy's bench entry is pulled onto the field, it's
already carrying a fresh hypothesis — exactly as if it had been seeded at
team preview. Re-arming only ever adds hypotheses back (widens the belief),
so it can't itself introduce a contradiction.

Because resolution can now happen more than once per battle, every promotion
site (Pass 1 move/item reveals, the Pass 5 backstop, and `IllusionEnded`) also
calls `remove_stale_zoroark_bench_duplicate` right after
`finish_illusion_promotion_restore`: the side's own pre-existing bench entry
for this same physical Zoroark (seeded at team preview, or re-attached by a
prior `rearm_zoroark_on_side`) is now a stale duplicate of the mon that just
resolved, and left in place would both trip `enforce_unique_item`'s
same-item-twice check and could later be mistaken by `rearm_zoroark_on_side`
for a second, still-unresolved Illusion forme.

---

## The Materialize Bridge (`materialize.rs`)

Pass 3's damage oracle needs concrete `&BattleState`/`&PokemonState` values;
all it has is `UnknownBattleState`/`UnknownPokemonState`. `materialize_pokemon`
and `materialize_battle` do the mechanical translation for one candidate
hypothesis at a time — callers are responsible for enumerating the right set
of hypotheses; this module just performs the field mapping faithfully.

`materialize_pokemon(unk, stats_override, item, ability)` overrides the
entire stat array with the candidate BSV-after-nature values being tested and
sets every other field from whatever `unk` currently knows, defaulting
harmlessly where a field is still unknown and doesn't affect damage (e.g. an
unresolved nature becomes `Hardy`, since the stats are already overridden
directly). The one load-bearing default is **HP**: a display of exactly 100%
maps to the candidate's max HP (so full-HP-gated reducers like Multiscale can
be tested active); any other percentage maps to a fixed 0.5×max-HP sentinel,
strictly below max HP, so those same reducers correctly read as inactive and
aren't double-counted against the unconditional allowlist Pass 3 already
applies. That sentinel sits *above* the ≤⅓ HP pinch-ability gate (Blaze,
Overgrow, Swarm, Torrent), so Direction B enumerates pinch-active HP as an
explicit separate hypothesis when the attacker's displayed percent could be in
that range (S25): ≤31% tests pinch-active only, 32–34% (where the display
bucket straddles the gate) tests both and unions the results, and ≥35% (or
100%, or an exact `Number`) uses the sentinel path alone.

`materialize_battle(unk, p1_active, p2_active)` copies weather/terrain/side/
slot conditions directly and maps every unresolved field timer to an
arbitrary `3` — safe because the oracle only ever checks whether an effect is
*currently active*, never its remaining duration. `Known(0)`, the sentinel for
a permanent effect (primordial weather, entry hazards), is handled in its own
match arm ahead of that fallback so it's never folded into it.

---

## Soundness Guarantee

The engine never excludes a training (EV/IV/nature/item/ability assignment)
that could actually have produced the observed events. Where multiple
explanations remain consistent, it keeps their **union**, not a guess. It only
narrows once an explanation is *provably* inconsistent with what was observed.

If the observed events are jointly impossible under any assignment — which
should never happen for a legally-generated event stream — the engine panics
via `inference_contradiction!` with a descriptive message identifying the
conflicting values. That panic is a signal of a bug in the event stream (an
emission-side or engine-side defect), not a normal inference outcome; every
soundness bug found this way and its regression test is logged in `AUDIT.md`.

---

## `InferenceConfig`

Passed to `apply_information` to control inference behaviour:

| Field | Default | Effect |
|---|---|---|
| `use_stat_points` | `true` | Restrict EV candidates to the 33-value `EV_LATTICE` instead of the full 0–252 range |
| `force_max_ivs` | `true` | Assume IVs are 31 (skips IV uncertainty entirely) |
| `level` | `50` | Level assigned to a newly-observed opponent Pokémon |
| `legal_items` | `None` | Optional item whitelist; a revealed item outside it panics. `None` = every item possible |
| `allow_repeat_items` | `false` | `false`: each non-`None` item can appear on at most one teammate, and confirming it on one excludes it from the rest. `true`: no cross-teammate exclusion |
| `learnset_dex` | `{}` | Per-species legal movesets; non-empty enables learnset-based Illusion narrowing |
| `ev_total_cap` | `Some(510)` | Total EV budget used for Pass 5's cross-stat ceiling tightening; `None` disables it |

---

## Testing

`src/tests/inference_tests.rs` exercises the engine end-to-end: constructing a
partial-information battle state, feeding it a scripted event sequence, and
asserting on the resulting bounds. Regression tests for every soundness fix in
`AUDIT.md` live there, named `test_s<NN>_*` / `roundtrip_s<NN>_*` after the
finding they cover.
