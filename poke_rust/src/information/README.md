# `information/` — Fog-of-War Inference Engine

This module translates raw, player-visible battle events into tightened bounds on
an opponent's hidden attributes (EVs, IVs, nature, item, ability, species). It has
two distinct jobs:

1. **Event model** (`information.rs`, `unknowns.rs`) — define what a player can observe
   and how partial knowledge is represented.
2. **Inference engine** (`inference.rs`, `materialize.rs`) — consume an ordered list of
   events and update the fog-of-war state through six passes.

---

## Part 1 — What a Player Sees: `Vec<InformationEvent>`

### The nested tree model

Every observable occurrence is an `InformationEvent`:

```rust
pub struct InformationEvent {
    pub kind: EventKind,
    pub reactions: Vec<InformationEvent>,
}
```

The `reactions` field is the key structural choice: **child events are nested inside
the event that caused them**, rather than emitted as a flat sequence with a cause tag.
This means the cause is always implicit from the parent, and the inference engine can
read item/ability/secondary effects in context without back-referencing earlier entries.

A Life Orb + pinch-berry + drain scenario looks like:

```
MoveUsed { user: P1[0], move: DrainPunch, targets: [P2[0]] }
  ├── Crit { target: P2[0] }
  ├── DamageDealt { target: P2[0], new_hp: Percent(38) }          ← PRE-berry HP
  ├── Healed { target: P2[0], new_hp: Percent(56) }               ← Sitrus Berry heal
  ├── ItemLost { slot: P2[0], item: SitrusBerry, consumed: true } ← from the item ledger
  ├── Healed { target: P1[0], new_hp: Number(185) }               ← drain
  └── DamageDealt { target: P1[0], new_hp: Number(162) }          ← Life Orb recoil
```

A pinch/HP berry (Oran, Sitrus, Figy, …) that fires mid-hit is emitted as its own
`Healed` reaction reporting the **post-berry** HP, immediately after a `DamageDealt`
that reports the **pre-berry** HP — never folded into one combined `DamageDealt`,
which would understate the true damage dealt (Pass 3's damage-to-stat inference reads
this delta directly). The berry's own `ItemLost` is a separate sibling, emitted
generically later by a whole-move item-snapshot diff rather than nested under the
specific `Healed` it explains — a real player sees the same three lines in this order.
A damage-*reducing* item (a type-resist berry like Occa Berry) is different: it has no
HP-change event of its own at all, since its effect is baked directly into the single
damage roll.

The inference engine walks this tree depth-first. Every `DamageDealt` nested under a
`MoveUsed` automatically carries context (which move, which user, which targets) from
its parent.

### HP representation: `PokemonHP`

HP amounts are typed by visibility:

```rust
pub enum PokemonHP {
    Number(u16),   // own Pokémon — exact HP
    Percent(u8),   // opponent — display percentage (0–100)
}
```

This matches what a real player sees. The inference engine exploits exact HP (Direction B)
for tight bounds and works from percent intervals (Direction A) where only the display
rounding is available.

### What events exist

`EventKind` covers every category of player-visible information:

| Category | Examples |
|---|---|
| Major actions | `MoveUsed`, `Switch`, `SimultaneousSwitch`, `Faint`, `EndOfTurn` |
| Form changes | `MegaEvolution`, `Terastallization`, `FormeChange`, `TypeChanged` |
| HP changes | `DamageDealt`, `Healed`, `SetHp` |
| Hit qualifiers | `Crit`, `Immune`, `Missed`, `MoveFailed`, `Blocked`, `HitCount` |
| Status | `StatusInflicted`, `StatusCured` |
| Stat stages | `BoostChanged`, `BoostsCleared`, `BoostsInverted`, `BoostsSwapped` |
| Field | `WeatherChanged`, `TerrainChanged`, `PseudoWeatherStart/End` |
| Side/slot | `SideConditionStart/End`, `SlotConditionStart/End` |
| Volatiles | `VolatileStart`, `VolatileEnd`, `PerishCount` |
| Items | `ItemRevealed`, `ItemGained`, `ItemLost` |
| Abilities | `AbilityRevealed` |

---

## Part 2 — How Partial Knowledge Is Stored: `UnknownBattleState`

### The `Unknown<T>` lattice

Every hidden attribute uses a three-variant enum:

```rust
pub enum Unknown<T> {
    Known(T),          // definitively identified
    Not(Vec<T>),       // could be anything except these excluded values
    Possibly(Vec<T>),  // must be one of these candidates
}
```

`Known` is the narrowest; `Not([])` is the widest (no exclusions). The lattice only
ever moves toward `Known` — the soundness invariant is that the true value is always
within the `Unknown` at every point in time.

`Possibly` is created specifically for Zoroark/Illusion scenarios via
`maybe_widen_for_illusion`: when a Pokémon might be disguised, its species becomes
`Possibly([actual, disguise_target])`. Learnset narrowing then removes candidates
that can't learn an observed move.

### Per-Pokémon state: `UnknownPokemonState`

For an opponent Pokémon, every attribute that cannot be directly seen is wrapped in
`Unknown<T>` or represented as a min/max range:

```
possible_species:            Unknown<Species>
possible_types:              Unknown<Vec<PokemonType>>
item:                        Unknown<Item>
possible_natures:            Unknown<Nature>
possible_abilities:          Unknown<Ability>
possible_weight_hg:          Unknown<u16>    ← 1:1 with species
possible_tera_type:          Unknown<PokemonType>

minEvs / maxEvs:             [u8; 6]         ← per-stat EV bounds
minIvs / maxIvs:             [u8; 6]         ← per-stat IV bounds
minStats / maxStats:         [u16; 6]        ← derived stat ranges
min_pre_nature_stat /
max_pre_nature_stat:         [u16; 6]        ← BSV bounds (before nature ×)
```

`min_pre_nature_stat` and `max_pre_nature_stat` store the **pre-nature base stat
value** — the result of `calc_stat(base, iv, ev, level, 1.0)` before the ×0.9/1.0/1.1
nature modifier. Pass 3 writes to these directly from observed damage, and Pass 5
reads them to back-solve EV/IV ranges.

A newly seen opponent mon is initialised by `from_opponent_species`:
- `minStats[i]` = `calc_stat(base[i], 0, 0, level, 0.9)` (worst case per stat independently)
- `maxStats[i]` = `calc_stat(base[i], 31, 252, level, 1.1)` (best case)
- Items/abilities/nature = `Not([])` (nothing excluded yet)

An own Pokémon is initialised by `from_known_pokemon`: every `Unknown` is set to
`Known(…)` and every range collapses to a single value.

### The CNF predicate store

`UnknownBattleState::predicates: Vec<Vec<Statement>>` holds constraints that cannot
be committed to a field yet. The outer `Vec` is a conjunction (AND); each inner
`Vec<Statement>` is a disjunction (OR). This is standard conjunctive normal form (CNF):

```
predicates = [(A ∨ B), (C ∨ D ∨ E), …]
           = (A ∨ B) ∧ (C ∨ D ∨ E) ∧ …
```

A clause like `[HasItem(QuickClaw), HasAbility(QuickDraw), SpeedComparison{…}]` means
"at least one of: this mon holds Quick Claw, has Quick Draw, or is genuinely faster."

### `Statement` variants

| Statement | Meaning |
|---|---|
| `HasItem { mon_idx, item }` | Mon holds this item |
| `HasAbility { mon_idx, ability }` | Mon has this ability |
| `HasMove { mon_idx, pokemon_move }` | Mon knows this move |
| `HasStatus { mon_idx, status }` | Mon is afflicted with this status |
| `NatureBoostsStat { mon_idx, stat }` | Mon's nature gives +10% to stat |
| `NatureNerfsStat { mon_idx, stat }` | Mon's nature gives −10% to stat |
| `EVIVStatGE { mon_idx, stat, value }` | Pre-nature BSV ≥ value |
| `EVIVStatLE { mon_idx, stat, value }` | Pre-nature BSV ≤ value |
| `SpeedComparison { fast_idx, slow_idx, fast_mult, slow_mult }` | `spe(fast)*fast_mult ≥ spe(slow)*slow_mult` |
| `WeatherTurns / PseudoWeatherTurns / SideConditionTurns { turns }` | Field timer is this value |

`mon_idx` is a flat index over all mons in order:
```
[p1_active…, p1_known_back…, p1_possible_back…,
 p2_active…, p2_known_back…, p2_possible_back…]
```

### `mon_idx` helpers

```rust
mon_idx_for_active_slot(state, &FieldSlot) -> Option<usize>
get_mon_by_idx(state, idx)     -> Option<&UnknownPokemonState>
get_mon_mut_by_idx(state, idx) -> Option<&mut UnknownPokemonState>
```

---

## Part 3 — The Six-Pass Inference Pipeline

Entry point: `apply_information(state, events, …, config)`.

The pipeline runs on each call and visits all six passes in order. Passes are not
re-entrant within a single call, but Pass 6 (BCP) loops to fixpoint, and Pass 4
is intentionally run first to pre-warm speed bounds before Pass 3 needs them.

```
apply_information_battle
  ├── Pass 4 (speed ordering → Spe bounds)    ← run FIRST
  ├── propagate_speed_comparisons()            ← immediate fixpoint
  ├── [event walk: Passes 1–3 per event]
  ├── Pass 5 (back-solve EV/IV/nature)
  └── Pass 6 BCP to fixpoint
         └── propagate_speed_comparisons()     ← also inside BCP loop
```

---

### Pass 1 — Structural / Direct Facts

**Where:** `process_battle_event`, the depth-first walk of every `InformationEvent`.

This pass updates `Unknown<T>` fields directly from what is explicitly stated in the event:

- `AbilityRevealed` → `unknown_set_known(&mut mon.possible_abilities, …)`
- `ItemRevealed` → `unknown_set_known(&mut mon.item, …)`
- `ItemLost { consumed: true }` → sets `consumed_item`; `consumed: false` → sets `item_lost`
- `StatusInflicted` / `StatusCured` → sets `mon.status`
- `BoostChanged` → updates `mon.boosts[boost_idx]`
- `WeatherChanged` / `TerrainChanged` / `PseudoWeatherStart` → updates field state
- `Switch(state)` / `SimultaneousSwitch` → records species, level, HP, status at entry;
  creates a new `UnknownPokemonState` via `from_opponent_species` if unseen before
- `Terastallization` → sets `mon.is_tera` and `mon.possible_tera_type`
- `MoveUsed` → calls `reveal_move_on_mon` to fill `known_moves`; also calls
  `narrow_species_by_learnset` (Pass 1.5, see learnset narrowing below)

Contradictions (e.g., `ItemRevealed` for a mon whose `item` is already `Known` to
something else) cause an immediate panic via `inference_contradiction!`.

#### Ability tracking — `possible_original_abilities` and `possible_abilities`

Every `UnknownPokemonState` carries two ability fields:

- `possible_original_abilities` — the mon's **innate** ability (one of the species' slot
  set from `dex[species].abilities`). Changes only on mega-evolution or forme change.
- `possible_abilities` — the **live** ability (may differ mid-battle after Trace, Mummy,
  Skill Swap, etc.; resets to `possible_original_abilities` on switch-out).

On first sight, both are initialised from `dex[species].abilities`:
- Non-empty dex entry: both become `Possibly([slot0, slot1, slotH])` (deduplicated).
- No dex data: both remain `Not([])` (no narrowing — unknown species, unknown ability).

**Transitions handled in Pass 1:**
- `AbilityRevealed` where the revealed ability is **in** the current candidate set →
  `unknown_set_known` (narrow to `Known`). If it is **outside** the set, a live
  ability-change occurred (Trace copied a foreign ability, Mummy, etc.) → overwrite
  `possible_abilities = Known(ability)`; `possible_original_abilities` is unchanged.
- `Switch` / `SimultaneousSwitch` of a previously seen mon → reset
  `possible_abilities := possible_original_abilities` (Trace/Skill-Swap effects don't
  persist across a switch).
- `MegaEvolution` / `FormeChange` → recompute **both** fields from the new species'
  ability set (mega/forme abilities are typically fixed singletons in the dex).

Because `unknown_is_excluded` treats anything outside a `Possibly` set as excluded,
narrowing abilities on first sight immediately prunes all impossible literals from
every BCP clause in later passes.

---

### Pass 2 — Item / Ability Presence and Absence from Behaviour

**Where:** `process_battle_event`, after processing each `MoveUsed` block or
`EndOfTurn` event. Each helper function is named `pass2_*` or `pass_eot_*`.

This pass emits CNF clauses from observable side-effects, covering cases where the
item or ability itself is not directly named but its presence or absence is deducible.

#### Presence clauses (item/ability confirmed by side-effect)

Reactive items and abilities are modelled with the **nested-reveal convention**: the
item or ability that caused a reaction is always emitted as an `ItemRevealed` /
`AbilityRevealed` event *nested inside* the move event that triggered it, not as a
bare effect. Pass 1 therefore pins presence directly; Pass 2 handles only absence.

**Life Orb recoil presence:**
- If a `DamageDealt { target == user }` reaction appears under a `MoveUsed` at the
  Life-Orb recoil fraction of the damage dealt, add `HasItem(LifeOrb)` to the clause.

#### Absence clauses

**Contact-reaction absence (Rocky Helmet / Rough Skin / Iron Barbs):**
- After a contact move (`MoveFlag::Contact`) hits the defender and produces no
  `ItemRevealed{RockyHelmet}` or `AbilityRevealed{RoughSkin|IronBarbs}` in the
  reaction tree, those three are excluded on the defender via `unknown_exclude` —
  **unless** any escape is possible: the attacker has Long Reach (bypasses contact
  reactions), Magic Guard (negates chip), or holds Protective Pads. Probabilistic
  contact reactions (Static, Flame Body, Poison Point) are never excluded on absence.

**Choice item (multi-move → excluded):**
- If a Pokémon uses two different moves in consecutive turns, it cannot have a Choice
  item. Each move that contradicts a Choice constraint is excluded via `unknown_exclude`.

**Bright Powder / Lax Incense from a 100%-accurate miss:**
- If a move with `AccuracyType::Percent(100)` misses and neither the user's accuracy
  stage is lowered nor the target's evasion is raised, emit:
  `[HasItem(BrightPowder), HasItem(LaxIncense)]` as a disjunctive clause.

**Powder-move immunity (non-Grass target):**
- A powder-flagged move (`MoveFlag::Powder`) that produces `Immune`/`Blocked` on a
  target with no known Grass type emits
  `[HasItem(SafetyGoggles) ∨ HasAbility(Overcoat)]` on the target.

**Guaranteed-status absence (`pass2_guaranteed_status_absence`):**
- A move whose secondary effect has `chance == 100` and `status == Some(s)` that
  hits the target (a `DamageDealt` is present, no `Missed`/`Blocked`) but produces no
  `StatusInflicted` emits a disjunction of unknown preventers for status `s` on the
  target. Decidable preventers (type immunity, already statused, Substitute, Safeguard,
  terrain) are ruled out first; only unknown preventers appear in the clause.
  `HasItem(CovertCloak)` and `HasAbility(ShieldDust)` are added only for secondary
  effects on damaging moves (`*from_secondary`). Ground-type paralysis immunity is
  gated to Electric-type moves only (Ground cannot be paralysed by Body Slam but can
  by Thunder Wave). If harsh sunlight is active and the status is Freeze, the clause
  is skipped entirely — Freeze is impossible in sun regardless of any ability.
  The ability lists are exhaustive per status:
  - **Burn**: Water Veil, Water Bubble, Thermal Exchange, Leaf Guard (sun-gated)
  - **Paralysis**: Limber, Leaf Guard (sun-gated)
  - **Poison**: Immunity, Pastel Veil, Leaf Guard (sun-gated), Flower Veil (Grass-target)
  - **Sleep**: Insomnia, Vital Spirit, Sweet Veil, Leaf Guard (sun-gated)
  - **Freeze**: Magma Armor, Leaf Guard (sun-gated), Flower Veil (Grass-target)

  After Pass 1 narrows abilities to the species set, BCP typically collapses this
  clause quickly.

**Prankster-immunity (`pass2_prankster_immunity`):**
- A status-category move targeting one of our **Known Dark-type** mons that produces
  `Immune`/`MoveFailed`/`Blocked` emits `[HasAbility(Prankster)]` on the user — a
  unit clause that BCP immediately forces to `Known(Prankster)`.

**Ground-move immunity (`pass2_ground_immune_clause`):**
- A Ground-type damaging move that produces `Immune` on an opponent mon whose types
  are `Known` and do not include Flying, and the mon has no MagnetRise/Telekinesis
  volatile, emits:
  `[HasItem(AirBalloon) ∨ HasAbility(Levitate) ∨ HasAbility(Eelevate) ∨ HasAbility(EarthEater)]`
  BCP typically resolves this to `Known(AirBalloon)` once the species ability set is
  narrowed (most species cannot have Levitate, Eelevate, or EarthEater).

---

### End-of-Turn Inference

Two helpers fire during the `EndOfTurn` event walk (`pass_eot_heal`,
`pass_eot_sand_immunity`).

**Leftovers / Black Sludge (`pass_eot_heal`):**
An opponent mon's HP increases at end-of-turn with no attributable cause (Aqua Ring,
Ingrain, Grassy Terrain, Wish, Leech Seed — all decidable from volatiles and field
state) emits:
- `[HasItem(Leftovers)]` for non-Poison-type targets.
- `[HasItem(Leftovers) ∨ HasItem(BlackSludge)]` for Poison-type targets.

The clause is gated on the item not already being `Known(None)` or consumed, and on
the mon's item not already being excluded for these values.

**Flame Orb / Toxic Orb (`pass_eot_self_status`):**
When an `EndOfTurn` event contains a `StatusInflicted{Burn}` or
`StatusInflicted{ToxicPoison}` reaction targeting an opponent mon that had no prior
status and whose item is not already `Known`, the item is forced to `Known(FlameOrb)`
or `Known(ToxicOrb)` respectively. These are the only sources of self-status
infliction at end-of-turn; there is no other EoT self-burn or self-toxic-poison
mechanism in the game.

**Sandstorm chip absence (`pass_eot_sand_immunity`):**
When Sandstorm is active and an opponent mon that is not Rock / Ground / Steel takes
no EOT chip damage, emit:
```
[HasItem(SafetyGoggles), HasAbility(SandVeil), HasAbility(SandRush),
 HasAbility(SandForce), HasAbility(Overcoat), HasAbility(MagicGuard)]
```
Literals already excluded from the mon's `possible_abilities` or `item` are omitted
before emitting (BCP would prune them anyway, but skipping them avoids spurious clauses).

---

### Pass 3 — Damage → Stat Bounds

**Where:** `pass3_damage_to_stats`, called per `MoveUsed` event after the full reaction
tree is walked (so all `DamageDealt` events and the crit flag are available).

This is the most computationally intensive pass. Instead of inverting the damage formula
analytically (fragile under 22 flooring steps), it uses the real simulator as a
**forward oracle**: enumerate candidate stat values, simulate, and keep only the ones
that produce the observed damage.

#### Skip conditions (moves with no stat signal)

Moves are skipped when their damage cannot be inverted to a single offensive/defensive
stat:
- Status-category moves
- OHKO moves
- Moves with a fixed damage override (e.g. Seismic Toss)
- Retaliation moves (Counter, Mirror Coat, Metal Burst, Comeuppance)
- Ambiguous-stat moves (Shell Side Arm, Photon Geyser)
- Beat Up (BP depends on party members' base Attack)

#### Direction B — opponent attacks your Pokémon (exact HP known)

When `target.hp` is `PokemonHP::Number`, the damage delta is exact:
```
exact_damage = pre_hp - post_hp
```

The oracle scans the attacker's **pre-nature base stat value** (BSV) range
`[min_pre_nature_stat[off_si], max_pre_nature_stat[off_si]]`. For each candidate BSV `v`:
1. Materialise the attacker at `stats[off_si] = floor(v * nature_mod)`.
2. Call `calculate_damage_outcomes_for_target_with_options` (the same damage function
   the simulator uses for actual battle resolution).
3. Keep `v` if any `(damage, crit, _)` outcome has `damage == exact_damage` and
   `crit == observed_crit`.

The feasible BSV range is intersected across all hits of a multi-hit move. Booster
items and abilities (Choice Band, Life Orb, etc.) are scanned as separate oracle
calls; the union of their feasible BSV ranges is taken (sound: wider). CNF clauses
`[HasItem(ChoiceBand) ∨ EVIVStatGE(…)]` are emitted for conditional tightening that
BCP can later resolve.

**Binary-search optimisation (`find_feasible_bsv_range_b`):**
Damage is monotone in the attacking BSV (higher Atk ⟹ strictly non-decreasing
damage), so the feasible set for any fixed (item, ability) combo is a *contiguous
interval*. `find_feasible_bsv_range_b` binary-searches for the bracket endpoints
(`max_roll(bsv) ≥ exact_damage` for the low end; `min_roll(bsv) ≤ exact_damage`
for the high end), then refines inward with a short linear walk to the first
exactly-feasible BSV. This reduces oracle calls per combo from O(range) to
O(log range) + constant, with correctness guaranteed by the monotonicity invariant.

#### Direction A — you attack the opponent (percent HP seen)

When `target.hp` is `PokemonHP::Percent`, the damage is only known through the two
display percentages (pre-hit and post-hit). For a candidate max HP `H`, both display
values are inverted to their exact raw-HP buckets via `percent_bucket` (the exact
inverse of `hp_to_percent`: round-half-up, 0 only at faint, 100 only at full,
clamped 1–99), and the damage band is:

```
damage_lo = max(1, pre_bucket.lo − post_bucket.hi)
damage_hi = pre_bucket.hi − post_bucket.lo
```

(S22: each display percent carries its own ±0.5% rounding — the previous
`[(δ−0.5)%, (δ+0.5)%]`-of-max-HP band treated the *delta* as a single rounding and
could exclude achievable damages, and with them the true defensive BSV, for
large-HP defenders whose pre-hit HP was itself rounded. A `pre` of 100 or `post` of
0 is display-exact and automatically shrinks that side's bucket to a point. An empty
bucket means the `H` hypothesis cannot display the observed percent at all and is
skipped.)

The oracle then scans the defender's BSV range: a BSV `v` is feasible if any outcome
falls in `[damage_lo, damage_hi]`.

**HP candidate enumeration (`achievable_defender_hp_values`):**
Rather than stepping in strides of 4, Direction A enumerates exactly the HP values
that are achievable by the defender's species given its IV/EV bounds and the stat-points
lattice. This eliminates the unsound exclusion of BSVs whose achievable HP values happen
to fall at off-stride positions.

The unconditional union over all candidate (HP, nature class, BSV, def-item, def-ability)
tuples gives `global_bsv_lo` and `global_bsv_hi`, which are written back to
`min_pre_nature_stat` and `max_pre_nature_stat`.

**Defensive item/ability union (soundness invariant):**
Direction A iterates over `defensive_damage_items(defender)` and
`defensive_damage_abilities(defender)` — complete allowlists of every modifier the
damage oracle implements on the defender's side. Completeness is a **soundness
invariant**: any reducer omitted from the lists causes `min_pre_nature_stat` to be
raised above the true value for defenders that could have that modifier (unsound
exclusion). The allowlists include:
- All 18 type-resist berries (Occa … Roseli + ChilanBerry)
- Eviolite, AssaultVest (stat multipliers, baked into stats before the oracle call)
- Multiscale, ShadowShield, TeraShell (full-HP-gated ×0.5)
- Filter, SolidRock, PrismArmor (×0.75 on super-effective hits)
- ThickFat, FurCoat, IceScales, Heatproof, WaterBubble, PurifyingSalt, DrySkin,
  FairyAura, Fluffy, PunkRock (type/category/flag-specific reductions)

A self-checking test (`test_sc_allowlist_completeness_cross_validation`) verifies
completeness: it runs the oracle for every ability/item the simulator knows and asserts
that any which changes damage output is in the corresponding list.

**E-B pruning (performance):**
Before iterating `def_items` × `def_abilities`, Direction A prunes entries that are
provably inert for the specific move (e.g., IceScales when the move is Physical;
type-resist berries whose type ≠ effective move type; PunkRock when the move has no
Sound flag). Every prune rule is conservative — it only drops entries that cannot
possibly change the oracle's output — so soundness is preserved.

**Direction A CNF predicates (nature-conditional tightening):**
For each nature class κ, Pass 3 also computes the feasible defensive-BSV interval
under **neutral gear** (no reducer item/ability) and emits conditional CNF clauses:
```
[not-κ] ∨ EVIVStatGE{bsv_lo_neutral} ∨ ⋁ HasItem(reducer) ∨ ⋁ HasAbility(reducer)
[not-κ] ∨ EVIVStatLE{bsv_hi_neutral} ∨ ⋁ HasItem(reducer) ∨ ⋁ HasAbility(reducer)
```
where reducers are drawn from the full `defensive_damage_items` / `defensive_damage_abilities`
allowlists. When BCP excludes all reducer alternatives and pins the nature, the bound is
forced to `min_pre_nature_stat`/`max_pre_nature_stat` directly.

#### Multi-hit handling

Pass 3 collects **all** `DamageDealt` reactions for the target, in order. Each hit
is run as an independent constraint on the same BSV lattice; the intersection across
hits yields tighter bounds than a single hit alone. For Triple Kick / Triple Axel /
Population Bomb, the per-hit BP override is computed from the hit index:
- Triple Kick: 10, 20, 30 (base BP = 10 + 10×hit_idx)
- Triple Axel: 20, 40, 60
- Population Bomb: 20 per hit

#### Speed-dependent BP (Gyro Ball, Electro Ball)

BP for these moves is a function of the speed ratio between attacker and target.
Because the relevant speed may be hidden, the oracle is called at **both endpoints**
of the unknown mon's speed range (`minStats[5]` and `maxStats[5]`); a BSV is kept
if it is feasible at *any* speed in the range (sound over-approximation). Because
Pass 4 runs first, these speed bounds are already tightened before Pass 3 needs them.

#### Stat formulas used

```
HP  stat = floor((2 × base + iv + floor(ev/4)) × level/100) + level + 10
Non-HP  = floor((floor((2 × base + iv + floor(ev/4)) × level/100) + 5) × nature_mod)
```

The pre-nature BSV (`calc_stat(base, iv, ev, level, 1.0)`) is the intermediate value
before the `× nature_mod` step; it is what `min_pre_nature_stat`/`max_pre_nature_stat`
bound.

---

### Pass 4 — Speed Ordering → Spe Bounds

**Where:** `pass4_speed_from_order`, called on the **top-level** event list (not
recursively), run before the event walk (and once more after BCP — see
"Post-BCP re-derivation" below).

Turn order reveals speed relationships. For each consecutive pair of `MoveUsed`
events in the same **effective priority bracket**:

#### Effective priority

Priority is normally the move's `MoveData::priority`. Grassy Glide gets +1 when
Grassy Terrain is active (deterministic, folded directly).

When a mon's `possible_abilities` is already `Known`, `fold_known_ability_priority`
folds the ability into the effective priority bracket before comparing moves:
- `Known(Prankster)` + Status-category move → base priority + 1
- `Known(GaleWings)` + Flying-type move + mon at full HP → base priority + 1
- `Known(Triage)` + move with `heal_fraction ≠ [0,0]` → base priority + 3

When the ability is **not** yet Known, these priority-lifting abilities become escape
disjuncts in the emitted clause (see "Emitted clause" below).

#### Trick Room

If Trick Room is active (`pseudo_weathers.contains(TrickRoom)`), the mon that moved
**first** is the **slower** one. `fast_idx` and `slow_idx` are swapped to compensate.

#### Speed multipliers (`compute_speed_multipliers`)

Deterministic multipliers are folded into `fast_mult` / `slow_mult` using a common
integer denominator:

| Factor | Multiplier | Condition |
|---|---|---|
| Boost stage +n | (2+n) / 2 | `boosts[4]` > 0 |
| Boost stage −n | 2 / (2+n) | `boosts[4]` < 0 |
| Paralysis | 1 / 2 | `status == Paralysis` |
| Tailwind | 2 / 1 | side has `SideCondition::TailWind` |

The resulting comparison is:
```
base_spe(fast) × fast_mult  ≥  base_spe(slow) × slow_mult
```

Hidden multipliers (Choice Scarf ×1.5, Iron Ball ×½, Swift Swim ×2 in rain, etc.)
are **not folded in** — they become escape disjuncts. Folding them in would make the
predicate too strong (unsound) when those items/abilities haven't been excluded.

**Snapshot timing (S4):** `compute_speed_multipliers` takes the boost stage,
paralysis, and Tailwind values as explicit arguments rather than reading them live
from `state`. `pass4_speed_from_order` builds these per-mover as it scans
`top_events` in order, maintaining a running snapshot (`spe_boost` / `paralyzed` /
`tailwind` maps, seeded from `state` at call time) that is updated after each
top-level event by deep-scanning its reactions for `StatusInflicted`/`StatusCured`,
`BoostChanged{boost_idx: 4}`/`BoostsCleared`/`BoostsInverted`, and
`SideConditionStart`/`End{TailWind}`. Each mover's `SpeedComparison` is then built
from the snapshot **as of the point in the scan just before its own `MoveUsed`** —
i.e. reflecting every earlier action's effects this turn (e.g. an earlier Thunder
Wave paralyzing this mon before it acted), not `state`'s value at Pass 4's call
time. Getting this wrong bakes an incorrect numeric factor into a `SpeedComparison`
that `propagate_speed_comparisons` then uses to derive hard Spe bounds — a
soundness risk, not just imprecision (a stale-multiplier bug of this kind can even
manufacture a spurious `EVIVStatGE`/`LE`-style contradiction). `BoostsSwapped` /
`BoostsCopied` are deliberately not tracked by the snapshot (which specific stats
moved isn't recoverable from the event alone); this is a documented, narrow residual
gap, not a soundness hole, since the escape disjuncts below remain sound regardless
of snapshot staleness — only the numeric `fast_mult`/`slow_mult` needed the fix.

#### Emitted clause

The clause for each pair is:

```
[SpeedComparison{fast_idx, slow_idx, fast_mult, slow_mult}]
 ∨ HasItem(QuickClaw)    on fast_idx
 ∨ HasAbility(QuickDraw) on fast_idx
 ∨ HasAbility(Prankster) on fast_idx  [only if move is Status-category]
 ∨ HasAbility(GaleWings) on fast_idx  [only if move is Flying-type AND fast_idx at full HP]
 ∨ HasAbility(Triage)    on fast_idx  [only if move has heal_fraction ≠ 0]
 ∨ HasAbility(Stall)     on slow_idx
 ∨ HasItem(ChoiceScarf)  on fast_idx
 ∨ HasItem(IronBall)     on slow_idx
 ∨ HasItem(LaggingTail)  on slow_idx
 ∨ HasItem(FullIncense)  on slow_idx
 ∨ HasAbility(SwiftSwim) on fast_idx  [only in Rain/Heavy Rain]
 ∨ HasAbility(Chlorophyll) on fast_idx [only in Sun/Extreme Sun]
 ∨ HasAbility(SandRush)  on fast_idx  [only in Sandstorm]
 ∨ HasAbility(SlushRush) on fast_idx  [only in Snow]
 ∨ HasAbility(SurgeSurfer) on fast_idx [only on Electric Terrain]
 ∨ HasAbility(Unburden)  on fast_idx  [only if fast_idx.item_lost]
 ∨ HasAbility(QuickFeet) on fast_idx  [only if fast_idx has a status]
```

Items/abilities that have been excluded from the mon's sets are not added to the
clause. A unit clause (after BCP removes false literals) forces the `SpeedComparison`
to be treated as unconditional.

#### Post-BCP re-derivation

After the main BCP loop runs, if BCP forced a priority-lifting ability to `Known`
(e.g., a `[Prankster]` unit clause from `pass2_prankster_immunity` was just
resolved), Pass 4 and `propagate_speed_comparisons` are re-run once more with the
updated ability knowledge. This tightens speed bounds that were previously left loose
because the priority escape was still present.

#### `propagate_speed_comparisons`

After Pass 4 emits clauses, `propagate_speed_comparisons` walks every
`SpeedComparison { fast_idx, slow_idx, fast_mult, slow_mult }` predicate that sits in
a **unit clause** and applies (S17: a `SpeedComparison` sharing its clause with live
escape disjuncts is conditional — the order may be explained by Quick Claw etc. —
and must not be enforced until BCP has excluded every escape and collapsed the
clause to unit):

```
fast_min × fast_mult ≥ slow_min × slow_mult
 →  fast.minStats[5] ≥ ceil(slow.minStats[5] × slow_mult / fast_mult)

fast_max × fast_mult ≥ slow_max × slow_mult
 →  slow.maxStats[5] ≤ floor(fast.maxStats[5] × fast_mult / slow_mult)
```

This is run to fixpoint immediately after Pass 4 and again inside the BCP loop.

---

### Pass 5 — Back-Solve EV / IV / Nature

**Where:** `pass5_back_solve`, called per mon with a `Known` species after the event walk.

Given `minStats[i]`/`maxStats[i]` (tightened by Passes 1–4), Pass 5 inverts the stat
formula to derive EV/IV/nature constraints.

#### HP (stat index 0, no nature modifier)

```
HP = floor((2 × base[0] + iv + floor(ev/4)) × level/100) + level + 10
```

Enumerate `iv ∈ [minIvs[0], maxIvs[0]]` (or just `{31}` with `force_max_ivs`) and
`ev ∈ EV_LATTICE`. Keep `(iv, ev)` pairs where `calc_hp(base, iv, ev, level)`
falls in `[minStats[0], maxStats[0]]`. The min/max of kept EV values tighten
`minEvs[0]`/`maxEvs[0]`.

#### Non-HP stats (stat indices 1–5)

For each stat, iterate over all remaining candidate natures. For a given nature with
modifier `m ∈ {0.9, 1.0, 1.1}`:

```
non-HP stat = floor(calc_stat(base[i], iv, ev, level, 1.0) × m)
            = floor(BSV × m)
```

A `(iv, ev)` pair is kept if the computed stat falls in `[minStats[i], maxStats[i]]`
AND the BSV falls in `[min_pre_nature_stat[i], max_pre_nature_stat[i]]`. A nature
is marked impossible if no `(iv, ev)` pair passes for any stat that nature touches.
Impossible natures are excluded via `unknown_exclude`.

The global `min_ev` / `max_ev` per stat is the union over all surviving natures.

#### EV total-cap cross-stat tightening

When `config.ev_total_cap = Some(cap)` (default 510 for competitive Pokémon Champions):

```
For each stat i:
  budget = cap - Σ_{j≠i} minEvs[j]
  maxEvs[i] = max EV_LATTICE value ≤ budget
```

This only tightens ceilings, never raises floors, so it is always sound. When two
stats have high known minimums, the remaining stats get tighter maxima.

#### EV lattice (`EV_LATTICE`)

In stat-points mode the legal EV values are a sparse lattice. The conversion is:

```
ev = max(0, 8 × stat_points − 4)
```

for `stat_points ∈ 0..=32`, yielding 33 values:
`{0, 4, 12, 20, 28, 36, 44, 52, …, 252}` (step 8 after the initial 0→4 gap).

---

### Pass 6 — BCP (Boolean Constraint Propagation)

**Where:** `run_bcp`, iterated to fixpoint.

BCP processes each clause in `state.predicates` using three rules:

1. **Remove false literals:** a literal is definitively false (e.g., `HasItem(ChoiceBand)`
   when `mon.item = Not([ChoiceBand, …])` already excludes it). Empty clause → panic.
2. **Drop satisfied clauses:** if any literal is definitively true, the clause is
   already satisfied and can be removed.
3. **Unit propagation:** if exactly one literal remains live and it is not a
   `SpeedComparison` (which is a relational constraint, not a field assignment), force
   it via `force_literal`.

`SpeedComparison` literals stay in the predicate store permanently as relational
constraints; `propagate_speed_comparisons` runs at the end of every BCP iteration
to extract concrete stat bounds from them.

`force_literal` for the various statement types:
- `HasItem` → `unknown_set_known(&mut mon.item, …)`
- `HasAbility` → `unknown_set_known(&mut mon.possible_abilities, …)`
- `HasMove` → `reveal_move_on_mon`
- `HasStatus` → `mon.status = Some(…)`
- `EVIVStatGE` → raise `mon.min_pre_nature_stat[si]`
- `EVIVStatLE` → lower `mon.max_pre_nature_stat[si]`
- `NatureBoostsStat` → narrow `possible_natures` to the boosting subset

---

## Learnset Narrowing (Illusion / species disambiguation)

**Where:** `narrow_species_by_learnset`, called at the end of the `MoveUsed` and
`ChargingMove` handlers in the event walk.

When a Pokémon's `possible_species` is `Possibly([s1, s2, …])` (typically set by
`maybe_widen_for_illusion` for Zoroark disguises) and a move is observed:

1. For each candidate species `s`, look up `learnset_dex.get(s)`.
2. If the learnset exists and does **not** contain `move_used`, remove `s` from the
   candidate set.
3. Species with no learnset data are kept (sound: absence of data is not evidence
   of inability).
4. If exactly one candidate remains, collapse to `Known(s)` and refresh:
   - `possible_types` → `Known(dex[s].types)`
   - `possible_weight_hg` → `Known(dex[s].weight)`

This is only sound because it only removes species that are provably unable to produce
the observed move. It never narrows on species that could plausibly learn it.

---

## The Materialize Bridge (`materialize.rs`)

The damage oracle (`calculate_damage_outcomes_for_target_with_options`) takes concrete
`&BattleState` / `&PokemonState` values. Pass 3 bridges this gap:

`materialize_pokemon(unk, stats_override, item, ability) -> PokemonState`
- Uses `unk.possible_species` (must be `Known`)
- Overrides the entire stats array with `stats_override` (the candidate BSV
  after applying the nature modifier)
- Sets `weight_hg` from `possible_weight_hg` — required for Low Kick / Heavy Slam BP
- **HP heuristic**: `Percent(100)` → `stats_override[0]` (max HP, enables Multiscale /
  ShadowShield / TeraShell); any other percent → `max_hp × 0.5`. The ×0.5 value is
  strictly less than max HP, so all three full-HP-gated reducers evaluate to "inactive"
  in the oracle. No double-count with the defensive allowlist union: those abilities are
  always enumerated by `defensive_damage_abilities`, but when HP is not 100% they
  contribute nothing because the oracle gates them.

`materialize_battle(unk, p1_active, p2_active) -> BattleState`
- Copies all field effects (weather, terrain, side conditions, pseudo-weathers, slot
  conditions) from `unk` to the concrete `BattleState`
- **Timer heuristic**: `Unknown::Known(t) => t` (exact); unknown timers → 3 turns.
  Timer values are **damage-irrelevant** — the oracle checks whether an effect is
  *present* (weather ≠ None, screen in side_conditions), not its remaining duration.
  `Known(0)` is the permanent-effect sentinel used for primordial weather and entry
  hazards; the `Known(t)` arm must remain first to prevent 0 from folding into the
  `_ => 3` fallback.

---

## Soundness Guarantee

The engine never excludes a training (EV/IV/nature/item/ability assignment) that could
actually have produced the observed events. When the engine cannot determine which of
several explanations is correct, it takes the **union** — keeping all possibilities.
Only when an explanation is provably inconsistent with observed events is it excluded.

If the observed events are genuinely impossible under any training (e.g., a move is
revealed that cannot exist for the observed species), the engine panics via
`inference_contradiction!` with a descriptive message. This represents a bug in the
event stream, not a normal inference outcome.

---

## `InferenceConfig`

Controls inference behaviour at call time:

| Field | Default | Effect |
|---|---|---|
| `use_stat_points` | `true` | Restrict EV candidates to the 33-value lattice |
| `force_max_ivs` | `true` | Assume IVs = 31 (competitive norm) |
| `level` | `50` | Level for newly observed opponent mons |
| `legal_items` | `None` | Optional whitelist; `None` = all items possible |
| `allow_repeat_items` | `false` | Item clause: `false` = each non-`None` item may appear at most once per team; once a teammate's item is confirmed, that item is excluded from every other roster member's lattice. `true` = no cross-teammate exclusion |
| `learnset_dex` | `{}` | Learnset data; empty = skip learnset narrowing |
| `ev_total_cap` | `Some(510)` | Total EV budget for cross-stat tightening |
