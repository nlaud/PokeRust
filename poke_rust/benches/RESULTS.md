# Benchmark Results

Produced by `cargo bench --bench turn_speed` (turn resolution) and
`cargo bench --bench solver_speed` (game-tree solver); see each bench's file
header for its scenarios. Latest recorded runs below — append a new section when
re-measuring after engine changes.

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

# Game-Tree Solver

## 2026-07-28 — solver landed

- Machine: Windows 11, 31.5 GB RAM, release build (bench profile)
- `cargo bench --bench solver_speed`; whole sweep ≈ 8 minutes
- Positions: real mid-turn-one states, built from `../teamsheets` pairings via
  team preview exactly as `turn_speed` does. Each cell averages over as many
  pairings as its turn-resolution budget allows, hence the varying `pairs`.
- `turns` = `simulate_turn` calls; `cells` / `total` = matrix cells evaluated
  against matrix cells that existed. Doubles is capped to 24 joint actions per
  player (`cap`), so its rows measure cost, not play quality.
- Cells the cost model predicted to be too expensive print `skipped` and are
  omitted below.
- **Count columns reproduce only to ~1% across runs.** Not a solver property:
  `coalesce_branches` drains a `HashMap` at every expansion level, so
  intermediate branch order varies and float addition is not associative — a
  successor's probability can land a few ulps apart on two runs of the same
  input, which is enough to reorder a near-tie and shift the tree the search
  walks. Backward induction, which prunes nothing, drifts as much as the pruning
  algorithms do, which is what identifies the transition function as the source.
  Values agree bit for bit or within one ulp. Treat sub-percent movement between
  runs as noise.

```
scenario algorithm          depth rolls chance     cap pairs      time    turns    cells  total    lps
singles  backwardInduction      1     1 enumerate    -    24    4.56ms       51       51     51      0
singles  backwardInduction      1     1 top4         -    24    4.57ms       51       51     51      0
singles  backwardInduction      1     1 top1         -    24    4.45ms       49       49     49      0
singles  backwardInduction      1     4 enumerate    -    24   16.00ms       72       72     72      0
singles  backwardInduction      2     1 enumerate    -     7  191.24ms     3.6k     3.6k   3.6k     24
singles  backwardInduction      2     1 top4         -     9  170.70ms     3.2k     3.2k   3.2k     22
singles  backwardInduction      2     1 top1         -    24  123.62ms     2.2k     2.2k   2.2k     17
singles  backwardInduction      2     4 enumerate    -     1     5.46s    34.0k    34.0k  34.0k    197
singles  backwardInduction      3     1 top1         -     1     6.17s   118.3k   118.3k 118.3k   1.2k
singles  serializedBounds       1     1 enumerate    -    24    4.77ms       60       43     43      0
singles  serializedBounds       1     1 top4         -    24    4.73ms       60       43     43      0
singles  serializedBounds       1     1 top1         -    24    4.69ms       54       43     43      0
singles  serializedBounds       1     4 enumerate    -    24   17.03ms      100       43     43      0
singles  serializedBounds       2     1 enumerate    -     3  334.42ms     6.6k      924    924     20
singles  serializedBounds       2     1 top4         -     4  329.75ms     6.6k      852    852     19
singles  serializedBounds       2     1 top1         -    18  232.30ms     4.3k      712    712     16
singles  serializedBounds       2     4 enumerate    -     1     9.71s    85.5k     8.9k   8.9k    184
singles  serializedBounds       3     1 top1         -     1    13.40s   273.2k    37.3k  37.3k    913
singles  doubleOracle           1     1 enumerate    -    24    3.77ms       36       36     49      0
singles  doubleOracle           1     1 top4         -    24    3.61ms       36       36     49      0
singles  doubleOracle           1     1 top1         -    24    3.69ms       34       34     47      0
singles  doubleOracle           1     4 enumerate    -    24   12.12ms       54       54     68      0
singles  doubleOracle           2     1 enumerate    -    22   94.69ms     1.7k     1.7k   2.3k     41
singles  doubleOracle           2     1 top4         -    24   89.96ms     1.7k     1.7k   2.2k     40
singles  doubleOracle           2     1 top1         -    24   65.03ms     1.1k     1.1k   1.6k     28
singles  doubleOracle           2     4 enumerate    -     2     2.60s    14.7k    14.7k  17.8k    556
singles  doubleOracle           3     1 top1         -     2     2.19s    40.0k    40.0k  53.4k   1.1k
doubles  backwardInduction      1     1 enumerate   24    24  203.26ms     1.5k     1.5k   1.5k      1
doubles  backwardInduction      1     1 top4        24    24  195.65ms     1.2k     1.2k   1.2k      1
doubles  backwardInduction      1     1 top1        24    24  182.90ms      865      865    865      1
doubles  backwardInduction      1     4 enumerate   24     2    16.83s    25.3k    25.3k  25.3k      1
doubles  serializedBounds       1     1 enumerate   24    24  243.71ms     2.5k      506    506      1
doubles  serializedBounds       1     1 top4        24    24  229.29ms     2.0k      506    506      1
doubles  serializedBounds       1     1 top1        24    24  205.52ms     1.2k      506    506      1
doubles  serializedBounds       1     4 enumerate   24     1    36.63s    89.0k      480    480      1
doubles  doubleOracle           1     1 enumerate   24    24   63.55ms      469      469    826      2
doubles  doubleOracle           1     1 top4        24    24   62.94ms      389      389    746      2
doubles  doubleOracle           1     1 top1        24    24   62.02ms      265      265    618      2
doubles  doubleOracle           1     4 enumerate   24     5     7.06s    11.9k    11.9k  12.3k      2
```

Double oracle against backward induction, matched settings, in turn resolutions:

```
scenario depth rolls chance        BI turns     DO turns  speedup
singles      1     1 enumerate           51           36    1.44x
singles      1     1 top4                51           36    1.44x
singles      1     1 top1                49           34    1.44x
singles      1     4 enumerate           72           54    1.34x
singles      2     1 enumerate         3.6k         1.7k    2.05x
singles      2     1 top4              3.2k         1.7k    1.92x
singles      2     1 top1              2.2k         1.1k    1.91x
singles      2     4 enumerate        34.0k        14.7k    2.31x
singles      3     1 top1            118.3k        40.0k    2.96x
doubles      1     1 enumerate         1.5k          469    3.16x
doubles      1     1 top4              1.2k          389    3.18x
doubles      1     1 top1               865          265    3.27x
doubles      1     4 enumerate        25.3k        11.9k    2.12x
```

## Takeaways

- **The solver's cost is `simulate_turn`, essentially entirely.** Across every
  row, time ≈ turns × a per-turn constant — ~50 µs for singles at one roll,
  ~160 µs at four, ~650 µs for doubles at four — while `lps` never exceeds ~1.2k
  even on the 118k-turn row, and a matrix LP is microseconds. Optimizations that
  cut turn resolutions win; optimizations that cut LPs do not. This inverts the
  assumption the source papers were written under, where transitions are cheap.
- **Double oracle pays off, and its margin grows with depth**: 1.44× at depth 1,
  ~2× at depth 2, 2.96× at depth 3, 3.1–3.3× in doubles. It is the default for
  that reason. Growth with depth is expected — the saving is per node, so it
  compounds.
- **Serialized alpha-beta bounds are a net loss here, as predicted, and the
  benchmark exists to have checked rather than assumed.** They do exactly what
  the theory says: matrix cells drop sharply (depth 3: 118.3k → 37.3k, a 3.2×
  cut; doubles depth 1: 1.5k → 506). But each bound costs a full auxiliary
  alpha-beta search over the same subtree, so turn resolutions more than double
  (depth 3: 118.3k → 273.2k) and wall-clock roughly doubles (6.29 s → 13.69 s).
  Trading turn resolutions for matrix cells is the wrong direction in this
  engine. Kept behind `SolverAlgorithm::SerializedBounds` and
  `use_serialized_bounds`, both off by default.
- **Depth 3 singles is affordable; depth 3 doubles is not.** Double oracle
  reaches depth 3 in 2.2 s with one outcome per chance node. Doubles costs
  ~3–4× a singles ply even capped at 24 joint actions, and uncapped it is two
  orders of magnitude worse — a full doubles matrix is ~250×250 cells, every one
  a turn resolution.
- **Damage rolls cost more than depth does, per unit of fidelity.** Singles
  depth 2 goes 1.7k → 14.7k turns moving from 1 roll to 4 (8.6×), while going
  from depth 2 to depth 3 at one roll costs 36× — but the depth-3 answer is
  worth far more than a finer damage distribution at depth 2. Prefer depth, and
  spend `ChanceMode` on keeping the fan-out down.
- **Peak RSS 87 MB across the whole sweep** (steady state ~20 MB), including the
  doubles-enumerate cells. The search is depth-first, so live memory is
  proportional to depth × branching rather than to the tree; the transposition
  table is entry-capped, and the turn cache is off by default.
- Pure equilibria are common in real positions — hence the ordered fast paths in
  `matrix.rs`. Note `lps` = 0 for every *singles* depth-1 row: at one ply those
  matrices are resolved entirely by the single-action, saddle-point and dominance
  checks without ever reaching the simplex. Doubles needs the simplex once or
  twice per solve even at depth 1, which is consistent with its larger matrices
  admitting genuinely mixed equilibria more often.
