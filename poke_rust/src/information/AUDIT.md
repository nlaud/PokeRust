# Inference Soundness Audit — Round 3 (S17–S29)

Scope: the fog-of-war information/inference system (`src/information/`) plus the
event-emission sites in `simulator/` it depends on. The audit looked for two
failure classes:

1. **Soundness errors** — the inferred state *excludes* a possible actual state
   (a training/item/ability/HP assignment that could have produced the observed
   events). These surface either as silently-wrong bounds/exclusions or as
   `inference_contradiction!` panics on perfectly legal game sequences.
2. **Drift** — the inferred state's *tracked* fields (HP, counters, flags,
   clause bindings) disagree with the simulator's actual state, so later
   inferences are computed against the wrong world.

Every FIXED finding below has a regression test (`test_s<NN>_*` in
`tests/inference_tests.rs`); S17–S20 and S23 were additionally reproduced
against the pre-fix source before committing.

---

## Fixed in this round

### S17 — Conditional SpeedComparisons were enforced as hard bounds  *(soundness, high)*

`collect_speed_comparisons` harvested `SpeedComparison` literals out of
**multi-literal** clauses, so `propagate_speed_comparisons` enforced them
unconditionally even while escape disjuncts (Quick Claw, Quick Draw, Choice
Scarf, Stall, weather abilities, …) were still live. Since a freshly seen
opponent's item is never excluded, *every* same-bracket move-order observation
immediately hard-tightened Spe bounds. A Quick Claw proc letting a slow mon move
before a fast known mon raised the slow mon's min Spe to the fast mon's speed —
excluding every true slower-with-Quick-Claw world — and panicked outright
whenever the forced min exceeded the species' maximum (empirically: Snorlax min
Spe forced 94 → 150).

**Fix:** only unit clauses propagate; BCP's per-iteration literal pruning
collapses a clause to unit once all escapes are excluded, exactly as the README
described the intended design.

### S18 — Slot re-binding inherited the old occupant's clauses  *(soundness + drift, high)*

An active `mon_idx` identifies a **slot**, not a Pokémon (S1 made the index
arithmetically stable, but the *binding* still changes on every switch).
Persisted `Statement`s and the weather/terrain/screen setter records all store
the slot index of the mon they were observed on. Across a switch they silently
re-bound to the incoming Pokémon:

- a unit `SpeedComparison` recorded for the outgoing Snorlax raised a fresh
  switch-in Aggron's min Spe from 94 to 150 (empirically reproduced);
- `[HasItem(BrightPowder) ∨ HasItem(LaxIncense)]` became an
  unsatisfiable-clause panic when the *new* occupant revealed a third item;
- a weather-timer collapse could reveal the old setter's Heat Rock as
  `Known` on the new occupant.

**Fix:** `purge_mon_scoped_knowledge` drops every clause referencing the slot
and nulls matching setter records at the moment the occupant leaves. Dropping
constraints only widens (sound); field-level knowledge still follows the
benched mon via `bench_outgoing_mon`.

### S19 — `HasItem` clauses evaluated against the wrong item epoch  *(soundness, high)*

`HasItem` clauses encode "held item X *at observation time*", but BCP evaluated
them against the mon's *current* item. After Knock Off / consumption / Trick,
stale clauses either panicked (`[BrightPowder ∨ LaxIncense]` then Knock Off →
both literals false → unsatisfiable; empirically reproduced) or were unsoundly
unit-forced (`[EVIVStatGE ∨ HasItem(OccaBerry)]` forced by the berry's own
consumption, raising the stat floor above the true berry-world value).

**Fix:** at every item-change event the mon's item literals are resolved
against the **outgoing** item: historically-true literals satisfy and drop the
clause; historically-false ones are pruned (which correctly collapses the
Damp Rock / weather-timer pair to the base-duration branch when the setter
turns out to have held a berry). Unknown outgoing item (rare `ItemGained` onto
an unresolved item) → purge, which is sound. The invariant "every surviving
clause was emitted during the current holding window" is maintained inductively
by running at every item change.

### S20 — Choice exclusion panicked on a Tricked-in Choice item  *(panic on legal play)*

Choice lock binds from the first move used *while holding* the item, so "used
move A, was Tricked a Choice Scarf, then legally picked move B" is consistent.
`pass1_choice_exclusion` counted two distinct moves this stint and excluded
`ChoiceScarf` from the mon's now-`Known(ChoiceScarf)` item — a guaranteed
`inference_contradiction!` (empirically reproduced). **Fix:** the exclusion is
gated on `!item_was_transferred`. (Item *loss* mid-stint needs no guard: the
exclusion no-ops on `Known(None)`.)

### S21 — Item-reaction absence treated as evidence while items may be inert  *(soundness, medium)*

The sim gates the Rocky Helmet chip and the Life Orb recoil on
`item_is_active` (Magic Room field-wide; Klutz on the holder). The absence
passes excluded `RockyHelmet` / `LifeOrb` without checking either, so a
Klutz+Helmet defender — or any holder during Magic Room — had its true item
excluded. **Fix:** the Helmet exclusion additionally requires no `MagicDeluge`
and Klutz excluded on the defender; the Life Orb absence path requires no
`MagicDeluge` and gains a `HasAbility(Klutz)` disjunct next to Magic Guard /
Sheer Force.

### S22 — Direction A damage band ignored the second display rounding  *(soundness, medium-high)*

The percent→damage conversion treated `δ = pre_pct − post_pct` as a single
display rounding (`[(δ−0.5)%, (δ+0.5)%]` of max HP), but pre and post each
round independently, so the true band is up to twice as wide. For large-HP
defenders (e.g. 362 max HP) whose pre-hit HP was itself rounded (anything but
exactly full), achievable damages fell outside the band, silently raising the
defensive-BSV floor above the true value. **Fix:** `percent_bucket` inverts
`hp_to_percent` exactly per max-HP hypothesis; damage ∈
`[pre_lo − post_hi, pre_hi − post_lo]`. This is sound *and* strictly tighter
where the display is exact (pre = 100 / post = 0 / single-value buckets), and
an empty bucket proves the max-HP hypothesis impossible (skipped). An
exhaustive cross-validation test checks every raw-HP pair at four max-HP
values against the real display convention.

### S23 — Pass 3 hit loop: global crit flag, heal-blind baselines, post-move HP  *(soundness, medium-high)*

Three related defects in the per-hit walk, all reproduced against the pre-fix
source:

1. **Per-hit crit** — one `Crit` reaction set a flag for *every* hit of a
   multi-hit move. A non-crit hit constrained to crit-only rolls has its
   feasible interval at observed-damage ÷ 1.5 — below the truth. With the
   corrupted hit first in sequence this capped the Atk bound under the true
   value and could panic Pass 5 ("every candidate nature infeasible").
2. **Interleaved heals** — a pinch berry firing between hits (emitted as its
   own `Healed`, per the S5/S6 emission convention) was skipped, so the next
   hit's baseline stayed pre-berry and its damage was understated by the heal.
3. **Post-move HP** — both direction oracles materialized the target from the
   live fog state, which Pass 1 has already advanced to the post-move HP, so
   full-HP-gated reducers (Multiscale / Shadow Shield / Tera Shell) were
   evaluated as inactive for exactly the hit that broke full HP.

**Fix:** the walk now processes `Crit` / `Healed` / `SetHp` / `DamageDealt` in
order with a pending-crit flag (matching the sim's single emit site: `Crit`
directly before its hit's `DamageDealt`) and threads each hit's true pre-hit
HP into the oracle materialization.

---

## Confirmed but not fixed (need design decisions)

### S24 — Pass 3 reads other post-move state (item / boosts / status)  *(soundness, medium)*

S23 fixed the HP half of this family; the rest remains. Pass 3 runs after the
full reaction tree has been applied by Pass 1, so the attacker/defender clones
it works from carry **post-move** state for an observation made **pre-move**:

- **Item**: a resist berry consumed *by the observed hit itself* pins the
  defender's item to `Known(None)` before `defensive_damage_items` runs, so
  the berry world is dropped from the union and the halved observed damage is
  explained by an inflated defensive BSV floor. Direction B mirrors this when
  *our* mon's berry was consumed by the observed hit (the oracle materializes
  the target with `Known(None)`, halving is unmodeled, and the attacker's max
  BSV drops below the truth).
- **Boosts**: a move with a self-boost or a target stat-drop secondary
  (Power-Up Punch, Overheat, Crunch) has the boost applied before Pass 3, so
  the oracle models the hit at the wrong stage.
- **Status**: Flame Body burning the attacker on contact halves the oracle's
  physical damage for a hit that was dealt unburned, shifting the feasible
  interval above the truth.

*Suggested fix:* extend `MoveContext` to snapshot the attacker's and each
target's `UnknownPokemonState` at `MoveUsed` time (the way `pre_hit_hp`
already does for HP) and run Pass 3 from those snapshots, writing bounds back
to the live mons.

### S25 — Materialize HP sentinel disables pinch abilities  *(soundness, medium)*

`materialize_pokemon` maps any non-100 display percent to `0.5 × max HP`. The
sentinel is deliberate for full-HP-gated *reducers*, but Blaze / Overgrow /
Swarm / Torrent gate at **≤ 1/3** in the sim (`hp*3 <= max`), so an opponent
attacker genuinely in pinch range is always modeled un-boosted. Observed
boosted damage then excludes the true (lower Atk + pinch ability) world even
though those abilities are enumerated in `offensive_damage_abilities` — the
enumeration is pointless while the materialized HP keeps them inactive.

*Suggested fix:* when the attacker's display percent bucket admits HP ≤ 1/3,
run the oracle at a pinch-active HP as an additional union branch (both
branches when the bucket straddles the threshold).

### S26 — Transform is invisible to inference  *(drift, medium)*

The simulator implements Transform (`transform_into`, helpers.rs) but emits no
identity-change event, and inference has no handler. After an opponent
transforms, its fog entry keeps the original species/stats/moves/ability while
the sim mon is a copy of the target: moves used while transformed are burned
into the original's `known_moves` slots permanently, Pass 3 inverts observed
damage against the wrong base stats (usually yielding empty feasible sets —
lossy — but capable of poisoning EV/nature back-solving), and the
Choice-exclusion / learnset logic reasons about moves the mon never owned.

*Suggested fix:* emit a dedicated event (or reuse `FormeChange
{permanent:false}` plus a marker) and give the fog state a
`pre_transform`-style snapshot/restore, which `UnknownPokemonState` already
has a field for.

### S27 — `consecutive_move_count` drifts on zero-damage outcomes  *(drift, low)*

The sim resets `consecutive_move_count` to 0 and nulls `last_used_move` when a
damaging move deals no effective damage (miss/immune/protect); inference
increments on every `MoveUsed` regardless of `Missed` / `Immune` / `Blocked`
reactions. The drifted streak feeds the Metronome-item multiplier in oracle
calls (it is materialized directly for our own attacker in Direction A) and
the Direction-B streak union starts from a wrong baseline.

*Suggested fix:* mirror the sim in the Pass 1 `MoveUsed` handler — if the move
is damaging and produced no `DamageDealt` on a non-user target, reset the
count and clear `last_used_move`.

### S28 — Analytic "moved last" heuristic misses non-move actions  *(soundness, low)*

Pass 3 decides whether Analytic fired by checking whether the *target* appears
in `move_users_this_turn`. A target that switched (or was fully paralyzed,
flinched, …) never appears, yet the attacker still moved last and Analytic's
×1.3 applied in the sim. Direction B then drops Analytic from the booster
union (the observed boosted damage forces the BSV floor above the truth);
Direction A substitutes `Ability::None` for our own genuinely-boosted
attacker, shifting the defender's feasible interval. In doubles the
target-centric test is also simply the wrong predicate ("all other actors
done", not "this target moved").

*Suggested fix:* track *all* consumed actions per turn (moves, switches,
`Cant` events) and treat "attacker is the last actor" as the Analytic
condition; when undecidable, union both branches.

### S29 — Illusion disguised as an already-benched teammate merges two mons  *(drift, low)*

`pass1_switch` pulls the benched fog entry whose species matches the incoming
`SwitchState.species`. When a Zoroark enters disguised as a teammate the
observer has *already scouted*, the real teammate's fog entry (HP, revealed
moves, item knowledge) is moved into the active slot and mutated by events
that physically happen to the Zoroark; on `IllusionEnded` the species is
overwritten but the merged knowledge stays, and the real teammate's entry has
been consumed from the bench. `maybe_widen_for_illusion` widens the species
set correctly, but the *state merge* is not undone.

*Suggested fix:* when the incoming species is widened for Illusion, keep the
benched entry in place and build the active entry fresh (species-only), then
reconcile on `IllusionEnded`.

---

## Checked and found sound (selected)

- **Weather/terrain ability-absence on same-weather entry** — `set_weather`
  deliberately re-emits `WeatherChanged` even when the incoming weather is
  already active, so absence of the event remains valid evidence (documented
  at helpers.rs `set_weather`).
- **Rocky Helmet / Rough Skin on a KO'd holder** — the sim emits the reveal
  and chip even when the holder faints ("Fires even if the holder faints"),
  and the reveal precedes the Magic Guard damage gate, so contact-absence
  inference needs no faint guard.
- **Intimidate vs Substitute** — the sim's Intimidate ignores Substitute, so
  the absence inference's visibility model matches the sim.
- **`pass_eot_heal` completeness** — Poison Heal is not implemented in the
  sim, so the Leftovers/Black Sludge/weather-ability disjunction is complete
  relative to the sim's EOT heal sources.
- **Tera and defensive typing** — the sim does not substitute `tera_type`
  into `mon.types` for defensive effectiveness or residual immunities, so
  inference's use of base `possible_types` is consistent with the sim.
- **Semi-invulnerability vs the Bright Powder clause** — the sim's
  invulnerability check bypasses the accuracy roll (no `Missed` emitted), so
  the 100%-accurate-miss clause is not triggered by Fly/Dig targets.
- **Regenerator vs Healing Wish** — the Healing Wish heal is emitted as a
  nested `Healed` under `SlotConditionEnd` after the `Switch` event's HP
  snapshot, so the bench-delta Regenerator inference does not misfire.
