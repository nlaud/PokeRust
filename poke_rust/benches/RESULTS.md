# Benchmark Results

Run these commands to measure turn resolution and solver speed:

```sh
cargo bench --bench turn_speed
cargo bench --bench solver_speed
```

The source files describe each test case. Add a section after an engine change.

## 2026-07-06: Sample mode

- Machine: Windows 11 with 31.5 GB of RAM
- Build: release benchmark profile
- Engine: `DamageConfig::sample` and `sample_turn`
- Singles: Aerodactyl uses Rock Slide against Pelipper in rain.
- Doubles: Both sides use two attacks. Two attacks hit both opponents.

The doubles test used more than 15 GB with 16 rolls and critical hits.

Slow enumeration results use one run. Fast results use an average.
Sample results use repeated runs for at least 0.4 seconds.
The `branches` column gives the number of weighted outcomes.

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

- Sample mode takes 0.07 to 1.75 ms per turn.
- One thread can sample about one million turns each minute.
- Singles enumeration takes at most about 22 ms.
- Doubles enumeration grows 16 to 20 times when the roll count doubles.
- Two doubles rolls with critical hits take 5.3 seconds and create about 65,000 branches.
- Critical-hit branches approximately double the branches for each damaging move.
- State copies and branch merging cause most enumeration cost.
- The benchmark omits settings that need excessive time or memory.

# Game-Tree Solver

## 2026-07-28: Solver

- Machine: Windows 11 with 31.5 GB of RAM
- Build: release benchmark profile
- Total test time: about eight minutes

The benchmark creates real first-turn states from pairs in `../teamsheets`.
Each result averages the number of pairs that fit its turn budget.

The `turns` column counts `simulate_turn` calls.
The `cells` column counts evaluated matrix cells.
The `total` column counts all matrix cells.
The doubles tests limit each player to 24 joint actions.
These tests measure cost, not play quality.

The benchmark omits tests that its cost model marks as too expensive.

Counts can vary by about one percent between runs.
`coalesce_branches` reads a `HashMap`, which has an unstable order.
Floating-point addition can then change a probability by a few units in its last place.
This small change can reorder close successors and change the search path.
Solver values agree exactly or differ by one unit in the last place.
Treat smaller count changes as noise.

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

This table compares double oracle with backward induction.
Each row uses the same settings and counts turn resolutions.

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

- `simulate_turn` causes almost all solver cost.
- One-roll singles turns take about 50 microseconds.
- Four-roll singles turns take about 160 microseconds.
- Four-roll doubles turns take about 650 microseconds.
- Fewer turn resolutions improve speed more than fewer linear programs.
- Double oracle becomes more effective as depth increases.
- Serialized bounds reduce matrix cells but increase turn resolutions and total time.
- `SolverAlgorithm::SerializedBounds` and `use_serialized_bounds` are disabled by default.
- Depth-three singles takes 2.2 seconds with one outcome at each chance node.
- Depth-three doubles is not practical.
- More search depth gives more value than more damage rolls.
- Peak memory use was 87 MB. Normal memory use was about 20 MB.
- Depth-first search keeps memory use proportional to depth and branching.
- Singles often have pure equilibria. Doubles need mixed equilibria more often.
