# Turn-Resolution Benchmark Results

Produced by `cargo bench --bench turn_speed` (see `turn_speed.rs` for the
scenarios). Latest recorded run below — append a new section when re-measuring
after engine changes.

## 2026-07-06 — sample mode landed

- Machine: Windows 11, 31.5 GB RAM, release build (bench profile)
- Engine state: sample mode (`DamageConfig::sample` / `sample_turn`) just added;
  `Missed`-event and doubles-endgame fixes included
- Scenarios: singles = Aerodactyl Rock Slide vs Pelipper Hurricane (rain);
  doubles = Rock Slide + Heat Wave spreads vs Hurricane + Draco Meteor —
  the turn shape that exceeded 15 GB under full enumeration at 16 rolls + crit
- Enumerate times: one run for slow configs, averaged for fast ones.
  Sample times: averaged over ≥ 0.4 s of repeated runs. `branches` is the
  number of weighted outcome states returned.

```
scenario mode       rolls  crit         time   branches
singles  enumerate      1 false       269 µs          5
singles  sample         1 false        68 µs          1
singles  enumerate      2 false       725 µs         14
singles  sample         2 false        81 µs          1
singles  enumerate      4 false      1.84 ms         44
singles  sample         4 false       112 µs          1
singles  enumerate      8 false      6.19 ms        152
singles  sample         8 false       170 µs          1
singles  enumerate     16 false      9.57 ms        226
singles  sample        16 false       280 µs          1
singles  enumerate      1  true       532 µs         14
singles  sample         1  true        86 µs          1
singles  enumerate      2  true      1.41 ms         35
singles  sample         2  true       114 µs          1
singles  enumerate      4  true      4.24 ms        101
singles  sample         4  true       166 µs          1
singles  enumerate      8  true     12.90 ms        329
singles  sample         8  true       290 µs          1
singles  enumerate     16  true     21.62 ms        517
singles  sample        16  true       521 µs          1
doubles  enumerate      1 false     16.50 ms        225
doubles  sample         1 false       168 µs          1
doubles  enumerate      2 false    287.26 ms       3850
doubles  sample         2 false       228 µs          1
doubles  enumerate      4 false       7.50 s      78630
doubles  sample         4 false       341 µs          1
doubles  enumerate      8 false      skipped          -
doubles  sample         8 false       626 µs          1
doubles  enumerate     16 false      skipped          -
doubles  sample        16 false      1.01 ms          1
doubles  enumerate      1  true    212.43 ms       2860
doubles  sample         1  true       230 µs          1
doubles  enumerate      2  true       5.32 s      64775
doubles  sample         2  true       314 µs          1
doubles  enumerate      4  true      skipped          -
doubles  sample         4  true       511 µs          1
doubles  enumerate      8  true      skipped          -
doubles  sample         8  true       920 µs          1
doubles  enumerate     16  true      skipped          -
doubles  sample        16  true      1.75 ms          1
```

## Takeaways

- **Sample mode is 0.07–1.75 ms per turn across every setting** — its mild
  growth tracks a single action's fan-out (briefly materialized before
  sampling), not the turn's combinatorics. Roughly 10⁶ sampled turns/minute
  single-threaded, which is the budget that matters for rollout-based search.
- **Singles enumeration is always cheap** (≤ ~22 ms at 16 rolls + crit,
  517 branches) — full-fidelity analysis in singles costs nothing.
- **Doubles enumeration grows ~16–20× per roll doubling** (two spread moves ×
  two targets compound the roll count four times over, before secondaries).
  2 rolls + crit is already 5.3 s / ~65k branches; 16 rolls + crit
  extrapolates to billions of branches — the observed >15 GB blow-up. The
  skipped cells are intentionally not run.
- **Crit branching ≈ 2× branches per damaging move**: a ~2× time bump in
  singles, ~13–18× in doubles (2× compounded across four move-target pairs).
- Enumeration cost ≈ branch count × (BattleState clone + coalesce hash);
  the clone-heavy state design is the constant factor, not the damage math.
