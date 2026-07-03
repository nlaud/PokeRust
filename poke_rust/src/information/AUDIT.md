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

Every finding below is FIXED, each with a regression test (`test_s<NN>_*` or
`roundtrip_s<NN>_*` in `tests/inference_tests.rs`). S17–S20 and S23–S29 were
additionally reproduced against the pre-fix source (or with the specific fix
logic neutered) before committing.

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

### S24 — Pass 3 read other post-move state (item / boosts / status)  *(soundness, medium)*

S23 fixed the HP half of this family. Pass 3 runs after the full reaction tree
has been applied by Pass 1, so the attacker/defender clones it worked from
carried **post-move** state for an observation made **pre-move**: a resist berry
consumed by the observed hit itself pinned the defender's item to `Known(None)`
before `defensive_damage_items` ran (dropping the berry world → inflated
defensive BSV floor; Direction B mirrored it for our own berry), a self-boost or
target stat-drop secondary (Power-Up Punch, Crunch) was applied before the
oracle ran (wrong stage), and a mid-move Flame Body burn halved the modeled
physical damage for a hit dealt unburned.

**Fix:** `MoveContext` now snapshots the attacker's and each target's full
`UnknownPokemonState` at `MoveUsed` time (alongside `pre_hit_hp`); both Pass 3
directions enumerate items/abilities and materialize from the snapshots, with
S23's per-hit HP override applied on top, while still writing bounds back to the
live mons. Pass 2's post-move reads (Life Orb ability reads, contact-absence
item reads) were audited and documented as sound at that epoch.

### S25 — Materialize HP sentinel disabled pinch abilities  *(soundness, medium)*

`materialize_pokemon` maps any non-100 display percent to `0.5 × max HP`. That
sentinel sits above the ≤1/3 gate used by Blaze / Overgrow / Swarm / Torrent,
so an opponent attacker genuinely in pinch range was always modeled un-boosted
and observed boosted damage excluded the true (lower-Atk + pinch) world.

**Fix:** Direction B derives the admissible gate states from the attacker's
display-percent bucket (reusing S22's `percent_bucket`): ≤31% → pinch-active
only, 32–34% → both hypotheses unioned, ≥35% / 100% / exact `Number` → the
sentinel path. The pinch hypothesis materializes a `Number` HP strictly inside
the gate and is unioned into the (item, ability, streak) enumeration and the
neutral-gear runs. (No other attacker-HP-gated damage modifier exists in the
sim — no Defeatist.)

### S26 — Transform was invisible to inference  *(drift, medium)*

The simulator implemented Transform (`transform_into`, from both the Transform
move and Imposter) but emitted no identity event and inference had no handler,
so a transformed opponent kept its original identity in the fog while the sim
mon was a copy: moves used while transformed were burned into the original's
`known_moves` permanently, Pass 3 inverted damage against the wrong base stats,
and Choice-lock / learnset logic reasoned about moves the mon never owned.

**Fix:** new `EventKind::Transformed { slot, into_slot, into_species }` emitted
from both `transform_into` call sites. Pass 1 saves the transformer into
`pre_transform` and overlays the copy source's fog identity
(species/types/weight/gender/ability/boosts/moves-with-PP-capped-5 and the five
non-HP stat bounds), so copying the observer's own Known mon yields exact stats
and copying a hidden opponent inherits its bounds; stale pre-transform clauses
are purged. Pass 5 and both Pass 3 inversions are skipped for a transformed mon,
and `apply_switch_out_reset` reverts the snapshot (preserving live
HP/status/fainted), mirroring the sim.

### S27 — `consecutive_move_count` drifted on zero-damage outcomes  *(drift, low)*

The sim resets `consecutive_move_count` to 0 and nulls `last_used_move` when a
damaging move deals no effective damage (miss/immune/blocked); inference
incremented on every `MoveUsed`, and the drifted streak fed the Metronome-item
multiplier in oracle calls (materialized directly for our own attacker in
Direction A).

**Fix:** the Pass 1 `MoveUsed` handler mirrors the sim — a damaging move with no
`DamageDealt` on a non-user target resets the count and clears `last_used_move`.

### S28 — Analytic "moved last" was decided by the wrong predicate  *(soundness, low)*

Pass 3 decided whether Analytic's ×1.3 applied by checking whether the *target*
had already moved this turn. A target that switched, flinched, or was fully
paralyzed never entered `move_users_this_turn`, so Analytic was wrongly judged
not to have fired (dropped from the Direction B booster union / substituted
`Ability::None` in Direction A) even when the attacker really moved last — the
observed boosted damage then forced the attacker/defender BSV bound past the
truth (roundtrip: `[178,182]` vs a true 150). It was also the wrong predicate in
doubles.

**Fix:** `compute_analytic_last_movers` precomputes, per turn segment (split on
`EndOfTurn`), the single slot that committed a move last (`MoveUsed` / `Cant` /
`MustRecharge` / `ChargingMove` — a `Switch` does not commit a move, matching the
sim's pending-`MoveAction` semantics). Analytic fires iff the attacker is that
slot; exact in singles and doubles.

### S29 — Illusion disguised as a scouted teammate merged two mons  *(drift, low)*

`pass1_switch` matched benched entries by species and **removed** the match, so a
Zoroark switching in disguised as an already-scouted teammate moved the real
teammate's fog entry into the active slot to be mutated by events belonging to
the Zoroark, and the real teammate's entry vanished from the bench.

**Fix:** when an Illusion disguise is possible (a Zoroark forme is on the
switching side's known bench) and the incoming species is not itself a Zoroark
forme, `pass1_switch` leaves the benched entry untouched and builds a fresh
species-only active entry for `maybe_widen_for_illusion` to widen.
`bench_outgoing_mon` discards a still-ambiguous (`Possibly` species) mon on
switch-out instead of benching it — it could never be re-matched (bench lookup
is by `Known` species) and would otherwise double-count one physical mon in
`teammate_indices` / item-clause propagation.

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
