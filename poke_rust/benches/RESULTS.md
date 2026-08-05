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

## 2026-08-04: Evaluator training

- Machine: Windows 11 with 31.5 GB of RAM
- Build: release profile
- Total training time: about 67 seconds

`solver::eval` now scores a position from a named feature vector and a weight
vector.
Each feature is a P1 quantity minus the matching P2 quantity.
A mirrored position therefore negates every feature, and the logistic map
returns one minus the original score.
Side-swap symmetry holds for every weight vector, fitted or hand-set.

`bin/train_eval` produced both weight files.

```sh
cargo run --release --bin train_eval -- --positions 400 --label-depth 2 --seed 1
```

Both weight files held `eval::HAND_WEIGHTS` and `eval::HAND_POLICY_WEIGHTS`
when the run started.
A label comes from a search that scores its own horizon with the committed
weights, so the run below is one improvement step from the hand-set vectors.
Restore both files to the hand-set values before a rerun.

The corpus draws two teams from the usage cache, plays twelve turns of random
legal commands, and records the position before each turn.
A state hash removes a repeated position.
Each label is an exact solve at depth two, with no node budget.

### Value model

| Weights | Train loss | Train MAE | Held-out loss | Held-out MAE |
|---|---|---|---|---|
| hand-set | 0.6349 | 0.0800 | 0.6349 | 0.0694 |
| fitted | 0.6257 | 0.0637 | 0.6307 | 0.0623 |

`weights/eval_v1.json`:

```text
health           +1.3291
status           -1.0962
boosts           +0.1056
accuracy_evasion +0.0294
hazards          -1.1159
threat           +0.5347
guaranteed_kill  +0.0140
possible_kill    -0.0659
speed            +0.0819
protect          -0.0932
tera             +0.0866
mega             +0.2263
screens          +0.0980
```

### Policy model

| Weights | Train loss | Train top-1 | Held-out loss | Held-out top-1 |
|---|---|---|---|---|
| hand-set | 2.6222 | 0.257 | 2.3635 | 0.304 |
| fitted | 1.8474 | 0.229 | 1.7213 | 0.291 |

`weights/policy_v1.json`:

```text
damage      +2.1196
kill        -0.2239
accuracy    -0.4844
priority    +0.0452
faster      +0.1632
switch      +0.7825
protect     -0.4695
status_move +0.3782
```

### Evaluator cost

`cargo bench --bench solver_speed -- --leaf-cost` measures the evaluator alone.
It skips the sweep, so a weight change or a feature change costs seconds to
re-measure.

The position is the first battle state of `MA_dragonite_rain.txt` against
`MB_gyarados_volcarona.txt`, singles, fixed leads.

| Evaluator | One leaf |
|---|---|
| `even` | 2 ns |
| legacy weights | 3255 ns |
| `heuristic` | 3339 ns |
| `fitted` | 3353 ns |

`features` runs whatever the weights are, so the legacy row costs a full feature
vector. It measures the search shape, not the leaf cost. `even` is the floor.

| Evaluator | Depth-2 solve | Turns | Value |
|---|---|---|---|
| legacy weights | 202.02 ms | 3.0k | 0.5642 |
| `fitted` | 191.33 ms | 2.9k | 0.6421 |

The sweep table of 2026-07-28 was not re-recorded in this session.

## Takeaways

- One leaf costs about 3.3 microseconds, against about 50 microseconds for a
  one-roll singles turn.
- The whole feature vector is that cost. The weights are a dot product.
- The threat feature asks `get_possible_commands_for_active_slot` which moves a
  slot may still pick, so a move without PP scores nothing. That check is about
  a third of the leaf cost.
- A depth-2 solve of the sample position spends most of its 191 milliseconds in
  `simulate_turn`, so turn resolution still dominates.
- The sharper leaf values did not cost the search anything here. Double oracle
  reached fewer cells with the fitted weights than with the legacy weights.
- The fitted value weights beat the hand-set weights on both splits.
- `SolveConfig::eval` and `MctsConfig::eval` therefore default to `eval::fitted`.
- The health and hazard weights moved little, so the original scale was close.
- The threat weight rose from 0.30 to 0.53, so the matchup carries real signal.
- Both kill weights sit near zero.
- One damage roll gives one branch, so a certain kill and a likely kill differ
  only by accuracy.
- Random play rarely reaches a position where a kill is in range, so the corpus
  teaches those two features little.
- A larger corpus needs more damage rolls, or more decided positions.
- The policy fit lowered the held-out cross entropy from 2.36 to 1.72, and it
  lowered held-out top-1 agreement from 0.304 to 0.291.
- Cross entropy scores the whole mixture, and top-1 scores only the mode. The
  two measures disagree on 400 positions.
- `MctsConfig::policy_prior` therefore stays `false`. The flag changes only the
  widening prefix.

## 2026-08-04: Champions doubles training

`src/solver/TRAINING.md` holds the rerun procedure.

### The corpus format changed

The run of 2026-08-04 used the defaults of `bin/train_eval`, which were one
active Pokemon and three brought.
That setting is Champions singles.
`team_preview_state_from_team_strings` also applies `BattleMechanics::default()`,
which enables Terastallization.
Pokemon Champions has none.
The `tera` weight of that run therefore came from illegal positions.

The defaults now describe Champions doubles.

| Setting | Before | After |
|---|---|---|
| Active per side | 1 | 2 |
| Brought per side | 3 | 4 |
| Roster size | 3 | 6 |
| Terastallization | on | off |
| Mega Evolution | on | on |
| Usage table | Singles | Doubles |

### The label is approximate

An exact depth-2 doubles label did not finish in ten minutes.
A doubles side offers hundreds of joint actions, so the payoff matrix holds tens
of thousands of cells, and each cell costs one `simulate_turn`.

The label search now uses `ChanceMode::TopK(1)`, an action cap of 24, and
dominated-action pruning.
Each of the three makes the label approximate.
An approximate depth-2 label still searches deeper than the leaf that it teaches.

Measured on the release build with 20 workers:

| Positions | Median | Max | Mean | Rate |
|---|---|---|---|---|
| 60 | 23.1 s | 78.3 s | 26.8 s | 0.58 labels/s |

The rate divides the kept label count by the labeling stage wall time.
An earlier version summed the per-label times and multiplied by the worker
count, which overstated the rate by about two times.
Calibrate with more positions than workers.
A sample that fits in one wave measures the slowest label, not the rate.

Corpus collection is cheap against the labels.
24000 distinct positions cost 9.9 seconds.

### The evaluator uses 16 damage rolls

`EVAL_DAMAGE_ROLLS` rose from 1 to 16.

One roll made the kill mass zero or one, so `possible_kill` equalled
`guaranteed_kill` on every move that cannot miss.
The two features were collinear, and no corpus could weight them apart.
Both weights sat near zero after the 2026-08-04 run.

The damage function computes every multiplier once and then loops the rolls, so
the extra rolls add cheap steps rather than whole calculations.

| Evaluator | One leaf, 1 roll | One leaf, 16 rolls |
|---|---|---|
| `even` | 2 ns | 2 ns |
| legacy weights | 3255 ns | 5829 ns |
| `heuristic` | 3339 ns | 5891 ns |
| `fitted` | 3353 ns | 5935 ns |
| `fitted_mlp` | — | 5886 ns |

The leaf stays far below one turn resolution, which costs about 50 microseconds.

`fitted_mlp` reads the same feature vector as `fitted`.
The feature vector is the whole cost, so the network adds nothing measurable.

### The network model

`eval::Mlp` holds one hidden layer with a `tanh` activation and no bias term.
The score is `logistic(output . tanh(hidden . features))`.

`tanh` is an odd function, and neither layer carries a bias term.
A mirrored position therefore negates the hidden vector and the output, and the
logistic map returns one minus the original score.
Side-swap symmetry holds for every weight matrix.
Do not add a bias term.

`Mlp::seed` starts a fit near its linear input model.
The trainer uses the newly fitted linear weights for this input.

`eval::fitted_mlp` takes the default evaluator slot only when it beats the
linear fit on the held-out split by 0.002 mean absolute error.
