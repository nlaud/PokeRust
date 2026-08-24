# Benchmark Results

Run these commands to measure turn resolution and solver speed:

```sh
cargo bench --bench turn_speed
cargo bench --bench solver_speed
cargo bench --bench depth2_cost
cargo bench --bench doubles_probe
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

## 2026-08-05: The overnight training run

- Machine: Windows 11 with 31.5 GB of RAM
- Build: release profile
- Start: 2026-08-05 06:46 UTC
- Labeling time: 33643 seconds, about 9 hours and 21 minutes
- Workers: 20 of 22 cores

```
train_eval --positions 24000 --label-depth 2 --min-label-depth 1 \
  --label-chance top1 --label-max-actions 24 \
  --time-budget 36000 --workers 20 --seed 1
```

The collector found 24000 distinct positions in 9.9 seconds.
The labeling stage gave a depth-2 label to every position.
The run kept 24000 value labels and 23868 policy labels.
No label left the corpus.

The time budget of 36000 seconds did not stop the stage.
The stage averaged 0.71 labels per second against the calibrated 0.58.
Size the corpus so that the budget binds.
This run finished the corpus first, so the budget gave no protection.

### The feature variance

| Feature | Variance |
|---|---|
| `health` | 0.9353 |
| `status` | 0.0094 |
| `boosts` | 4.2072 |
| `accuracy_evasion` | 0.0349 |
| `hazards` | 0.0001 |
| `threat` | 1.1880 |
| `guaranteed_kill` | 1.9718 |
| `possible_kill` | 2.1609 |
| `speed` | 6.2744 |
| `protect` | 0.9228 |
| `tera` | 0.0000, constant |
| `mega` | 0.2892 |
| `screens` | 0.0434 |

`tera` reads constant because Champions turns Terastallization off.
Both sides report the same flag, so the difference is always zero.
The fitted `tera` weight of 0.1470 therefore comes from the hand value of 0.15
and the L2 penalty alone.
That weight scores nothing under Champions rules.

`hazards` also carries almost no variance.
Random play rarely sets a hazard.

### The kill features separated

The kill feature correlation was +0.8882.
A value at or above 0.99 would mean that the two features stayed collinear.
The 16 damage rolls therefore did the work that the last run needed.

| Feature | 2026-08-04 | This run |
|---|---|---|
| `guaranteed_kill` | +0.0140 | -0.0548 |
| `possible_kill` | -0.0659 | +0.1555 |

The pair now carries signal that neither weight held before.
The model prices the chance of a kill and it discounts the certainty of one.

### The value fit

| Model | Train loss | Train MAE | Held-out loss | Held-out MAE |
|---|---|---|---|---|
| `heuristic` | 0.5829 | 0.1399 | 0.5842 | 0.1429 |
| `fitted` | 0.5429 | 0.0950 | 0.5418 | 0.0957 |
| `fitted_mlp` | 0.5438 | 0.0973 | 0.5428 | 0.0980 |

`fitted` beats `heuristic` on the held-out split.
The accept rule therefore keeps this run.

| Feature | 2026-08-04 | This run |
|---|---|---|
| `health` | +1.3291 | +1.7183 |
| `status` | -1.0962 | -1.0168 |
| `boosts` | +0.1056 | +0.0988 |
| `accuracy_evasion` | +0.0294 | +0.0110 |
| `hazards` | -1.1159 | -0.9770 |
| `threat` | +0.5347 | +0.3210 |
| `guaranteed_kill` | +0.0140 | -0.0548 |
| `possible_kill` | -0.0659 | +0.1555 |
| `speed` | +0.0819 | +0.0972 |
| `protect` | -0.0932 | +0.0962 |
| `tera` | +0.0866 | +0.1470 |
| `mega` | +0.2263 | +0.2235 |
| `screens` | +0.0980 | +0.3107 |

The 2026-08-04 column comes from a singles corpus with Terastallization on.
Read it as a different format, not as an earlier fit of the same thing.

### The learning curve

| Training split | Samples | Held-out MAE |
|---|---|---|
| 25% | 4800 | 0.0959 |
| 50% | 9600 | 0.0957 |
| 75% | 14400 | 0.0958 |
| 100% | 19200 | 0.0957 |

Four times the data lowered the error by 0.0002.
The curve is flat, so more positions do not help the linear model.
A larger corpus is not the next step for the value fit.
A new feature is.

### The model choice

The network lost by 0.0022 mean absolute error against a margin of 0.0020.
`SolveConfig::eval` and `MctsConfig::eval` therefore keep `eval::fitted`.
`weights/eval_mlp_v1.json` still ships, so a later run can compare against it.

One hidden layer over 13 features did not beat a dot product on this corpus.
The flat learning curve gives the reason.
The corpus does not hold the signal that a wider model needs.

### The policy fit

| Model | Train loss | Train top-1 | Held-out loss | Held-out top-1 |
|---|---|---|---|---|
| hand | 5.4177 | 0.073 | 5.3730 | 0.081 |
| fitted | 3.6745 | 0.069 | 3.6667 | 0.079 |

The fit lowered the cross entropy by 1.71.
It did not raise the top-1 agreement.
`MctsConfig::policy_prior` therefore stays `false`.

Both top-1 values sit far below the 0.30 of the singles run of 2026-08-04.
A doubles side picks a joint action over two slots, so the action set is much
larger.
A top-1 match is harder to get, and the number does not compare across formats.

### The sweep

`cargo bench --bench solver_speed` ran against the new weights.
The sweep took about 11 minutes.

Do not compare this table row by row with the table of 2026-07-28.
Fifteen solver commits landed between the two runs.
Two of them change the action set that a row searches.
`2686bca` replaced the stride-based action cap, and `4bef201` added the
dominated-action pre-filter.
The turn counts moved because the action set moved, not because the evaluator
changed.

```
scenario algorithm          depth rolls chance     cap pairs      time    turns    cells  total    lps
singles  backwardInduction      1     1 enumerate    -    24   13.02ms      149      149    149      0
singles  backwardInduction      1     1 top4         -    24   13.05ms      149      149    149      0
singles  backwardInduction      1     1 top1         -    24   12.16ms      140      140    140      0
singles  backwardInduction      1     4 enumerate    -    24   68.48ms      226      226    226      0
singles  backwardInduction      2     1 enumerate    -     7     1.42s    19.0k    19.0k  19.0k    122
singles  backwardInduction      2     1 top4         -     9     1.24s    16.8k    16.8k  16.8k    105
singles  backwardInduction      2     1 top1         -    24  840.43ms    11.5k    11.5k  11.5k     81
singles  backwardInduction      2     4 enumerate    -     1    50.68s   203.7k   203.7k 203.7k   1.7k
singles  backwardInduction      3     1 top1         -     1    51.66s   770.4k   770.4k 770.4k   6.5k
singles  serializedBounds       1     1 enumerate    -    24   13.71ms      174      123    123      0
singles  serializedBounds       1     1 top4         -    24   13.57ms      174      123    123      0
singles  serializedBounds       1     1 top1         -    24   12.69ms      158      123    123      0
singles  serializedBounds       1     4 enumerate    -    24   72.61ms      327      123    123      0
singles  serializedBounds       2     1 enumerate    -     3     2.71s    38.8k    11.8k  11.8k    137
singles  serializedBounds       2     1 top4         -     4     2.46s    36.2k    10.4k  10.4k    121
singles  serializedBounds       2     1 top1         -    18     1.49s    21.7k     6.8k   6.8k     81
singles  serializedBounds       2     4 enumerate    -     1    96.59s   534.7k   143.2k 143.2k   1.7k
singles  serializedBounds       3     1 top1         -     1   132.66s     2.1M   462.9k 462.9k   6.3k
singles  doubleOracle           1     1 enumerate    -    24    6.35ms       62       62    135      1
singles  doubleOracle           1     1 top4         -    24    6.05ms       62       62    135      1
singles  doubleOracle           1     1 top1         -    24    6.14ms       61       61    132      1
singles  doubleOracle           1     4 enumerate    -    24   26.49ms       96       96    168      1
singles  doubleOracle           2     1 enumerate    -    22  346.82ms     5.0k     5.0k   8.8k     98
singles  doubleOracle           2     1 top4         -    24  333.11ms     4.8k     4.8k   8.5k     93
singles  doubleOracle           2     1 top1         -    24  228.36ms     3.3k     3.3k   5.9k     72
singles  doubleOracle           2     4 enumerate    -     2    12.10s    49.1k    49.1k  76.0k   1.4k
singles  doubleOracle           3     1 top1         -     2    13.05s   192.1k   192.1k 321.2k   5.0k
doubles  backwardInduction      1     1 enumerate   24    24  300.63ms     1.5k     1.5k   1.5k      1
doubles  backwardInduction      1     1 top4        24    24  277.01ms     1.3k     1.3k   1.3k      1
doubles  backwardInduction      1     1 top1        24    24  234.21ms      917      917    917      1
doubles  backwardInduction      1     4 enumerate   24     2    20.27s    24.9k    24.9k  24.9k      1
doubles  serializedBounds       1     1 enumerate   24    24  356.19ms     2.3k      578    578      1
doubles  serializedBounds       1     1 top4        24    24  322.13ms     2.0k      578    578      1
doubles  serializedBounds       1     1 top1        24    24  255.55ms     1.3k      577    577      1
doubles  serializedBounds       1     4 enumerate   24     1    33.41s    78.5k      576    576      1
doubles  doubleOracle           1     1 enumerate   24    24  112.27ms      524      524    905      3
doubles  doubleOracle           1     1 top4        24    24  102.29ms      461      461    843      3
doubles  doubleOracle           1     1 top1        24    24   84.47ms      323      323    706      3
doubles  doubleOracle           1     4 enumerate   24     5     7.67s    10.6k    10.6k  11.0k      5
```

The sweep asked for 108 cells and skipped 68.
It gives a reason for each skip.

1. `rolls>1 only informative under enumerate`. A chance mode that drops
   outcomes hides the extra rolls.
2. `over the estimated-cost ceiling`. The cost model refused the row.
3. `doubles beyond one ply`. A doubles row above depth 1 is not practical.

The pruning payoff table compares double oracle with backward induction at
matched settings.

```
scenario depth rolls chance        BI turns     DO turns  speedup
singles      1     1 enumerate          149           62    2.39x
singles      1     1 top4               149           62    2.39x
singles      1     1 top1               140           61    2.30x
singles      1     4 enumerate          226           96    2.36x
singles      2     1 enumerate        19.0k         5.0k    3.82x
singles      2     1 top4             16.8k         4.8k    3.52x
singles      2     1 top1             11.5k         3.3k    3.54x
singles      2     4 enumerate       203.7k        49.1k    4.15x
singles      3     1 top1            770.4k       192.1k    4.01x
doubles      1     1 enumerate         1.5k          524    2.79x
doubles      1     1 top4              1.3k          461    2.79x
doubles      1     1 top1               917          323    2.84x
doubles      1     4 enumerate        24.9k        10.6k    2.34x
```

Double oracle now saves more than it did on 2026-07-28.
The singles depth-2 speedup rose from about 2.0x to about 3.6x.
A larger action set gives double oracle more actions to leave out.

### The leaf cost after the run

The position is the first battle state of `MA_dragonite_rain.txt` against
`MB_gyarados_volcarona.txt`, singles, fixed leads.

| Evaluator | One leaf |
|---|---|
| `even` | 2 ns |
| legacy weights | 5860 ns |
| `heuristic` | 5919 ns |
| `fitted` | 5909 ns |
| `fitted_mlp` | 6099 ns |

One leaf costs about 5.9 microseconds against about 50 microseconds for a
one-roll singles turn.
Turn resolution still dominates the search.

| Evaluator | Depth-2 solve | Turns | Value |
|---|---|---|---|
| legacy weights | 215.46 ms | 3.0k | 0.5642 |
| `fitted` | 210.49 ms | 3.0k | 0.6817 |

The new weights did not cost the search anything on this position.
The reported `fitted` value moved from 0.6421 to 0.6817 against the run of
2026-08-04.
Two changes drive that move together, the new weights and the 16 damage rolls.
The `legacy` value held at 0.5642 across both runs, which checks that the
search itself did not drift.

## Takeaways

- The accept rule passed. `fitted` cut the held-out mean absolute error from
  0.1429 to 0.0957, about a third.
- 16 damage rolls broke the collinearity of the two kill features. Their
  correlation was 0.8882, well under the 0.99 threshold.
- The learning curve is flat across a 4x range of corpus size. More positions
  no longer help the linear model.
- One hidden layer over 13 features did not beat a dot product. The default
  evaluator stays `eval::fitted`.
- The next gain must come from a new feature, not from a longer run.
- `hazards` and `tera` carry almost no variance. Random play sets few hazards,
  and Champions turns Terastallization off.
- The policy fit lowered the cross entropy and it did not raise the top-1
  agreement, the same split result as the singles run.
- Sizing a corpus so that a time budget stops it does not work. The label rate
  rose from 0.58 to 0.71 per second, and the corpus ran out first.

## 2026-08-21: Field features and a real-team corpus

The first run of `runbook/refresh_and_train.py`. Two things changed together, so
this run does not compare cleanly against 2026-08-05. Read it as a new baseline.

1. Seven field and side-condition features joined the frame. `FEATURE_COUNT`
   moved from 13 to 20.
2. The corpus plays archived teams. `--teamsheet-dir` supplies 758 rosters that
   `runbook/refresh_and_train.py` fetched from a VGCPastes export, and
   `--teamsheet-mix 0.8` sends 80 percent of matchups to them.

### The run

```sh
python runbook/refresh_and_train.py --hours 4
```

| Setting | Value |
|---|---|
| Teams | 758 archived rosters, 80 percent mix |
| Usage cache | Season `Current`, refreshed 2026-08-20, 235 Doubles species |
| Label depth | 2, iterative deepening, `--min-label-depth 1` |
| Chance mode | `top1` |
| Action cap | 24 per player, dominance filter on |
| Workers | 20 |
| Budget | 4 hours |

The labeling stage produced 11,733 labels in 14,459 seconds. It kept 11,646
value labels and 11,604 policy labels.

| Depth | Labels |
|---|---|
| 2 | 11,609 |
| 1 | 37 |
| dropped, deadline expired | 87 |

Depth 2 covered 99.7 percent of the corpus. The calibration measured 0.53 labels
per second, and the run held 0.81 per second.

### The value fit

| Model | Train loss | Train MAE | Held-out loss | Held-out MAE |
|---|---|---|---|---|
| `heuristic` | 0.5590 | 0.1505 | 0.5584 | 0.1514 |
| `fitted` | 0.5075 | 0.0953 | 0.5088 | 0.0978 |
| `fitted_mlp` | 0.5112 | 0.1005 | 0.5130 | 0.1027 |

The accept rule passed. `fitted` beat `heuristic` by 0.0536 on the held-out
split.

Do not read 0.0978 against the 0.0957 of 2026-08-05. That run labeled 19,200
positions from generated rosters. This one labeled 11,646 from archived teams,
which is a different and harder distribution. The `heuristic` error rose over
the same pair, from 0.1429 to 0.1514, which says the same thing.

### The field features

| Feature | Variance | Fitted weight |
|---|---|---|
| `weather_edge` | 0.0945 | +0.1326 |
| `weather_control` | 0.2508 | +0.0554 |
| `terrain_edge` | 0.0004 | +0.1090 |
| `terrain_control` | 0.0000, constant | +0.0686 |
| `tailwind` | 0.0141 | +0.2254 |
| `guard_conditions` | 0.0003 | +0.0443 |
| `trick_room` | 0.1751 | +0.1216 |

Three of the seven carry real signal: `weather_control`, `trick_room`, and
`weather_edge`. The team corpus explains each number.

| Source | Teams of 758 |
|---|---|
| Drizzle, Drought, Sand Stream, Snow Warning | 301 |
| Trick Room | 306 |
| Tailwind | 442 |
| Electric Surge | 1 |
| Grassy, Misty, and Psychic Surge | 0 |
| Safeguard, Mist, Lucky Chant | 1 |

`terrain_control` reads constant because one team of 758 carries a terrain
setter. `guard_conditions` reads near zero for the same reason. Champions M-B
has no terrain archetype and almost no status guard, so both features are
correct and dormant. Their weights come from the hand value and the L2 penalty
alone, as `tera` does.

`tailwind` carries a large weight against a small variance. 442 teams hold the
move, but the feature reads only the turns that Tailwind stands on exactly one
side. A doubles corpus often has it up on both sides, which cancels.

### The kill features

The kill feature correlation was +0.9022, under the 0.99 threshold. The 16
damage rolls still separate the pair.

| Feature | 2026-08-05 | This run |
|---|---|---|
| `guaranteed_kill` | -0.0548 | -0.0410 |
| `possible_kill` | +0.1555 | +0.1747 |

### The learning curve

| Training split | Samples | Held-out MAE |
|---|---|---|
| 25% | 2,329 | 0.0980 |
| 50% | 4,659 | 0.0978 |
| 75% | 6,988 | 0.0978 |
| 100% | 9,317 | 0.0978 |

Four times the data lowered the error by 0.0002. The curve stays flat, as it did
on the generated corpus. A larger corpus is not the next step.

### The model choice

The network lost by 0.0049 mean absolute error against a 0.0020 margin.
`SolveConfig::eval` and `MctsConfig::eval` keep `eval::fitted`.

One hidden layer over 20 features did not beat a dot product, the same result
that 13 features gave.

### The policy fit

| Model | Train loss | Train top-1 | Held-out loss | Held-out top-1 |
|---|---|---|---|---|
| hand | 5.4557 | 0.073 | 5.3557 | 0.069 |
| fitted | 3.7062 | 0.071 | 3.6892 | 0.068 |

The fit lowered the cross entropy by 1.67 and did not raise the top-1
agreement. `MctsConfig::policy_prior` stays `false`.

### What the refresh broke

The refresh moved the cache, and three tests read cache contents rather than
loader behavior. Each one documented that risk and then pinned a value anyway.

1. `Mega Gallade` joined the roster. `SPECIES_OVERRIDES` now maps it to
   `Species::GalladeMega`. `runbook/check_species.py` lists every unresolved
   name in one pass.
2. `loads_the_entire_cache` pinned the species count at 235. It now asserts a
   range. It also pinned unmapped names at zero, and the site sent four corrupt
   `stat_alignment` strings. It now allows a small count, which still catches
   enum drift.
3. `tolerates_species_with_missing_categories` pinned Ditto. No species has a
   category gap this season. The test now reads whatever the cache holds.
4. `every_cache_species_has_a_champions_learnset` counted every cache species.
   A Mega forme has no learnset and `is_selectable_species` already drops one,
   so the test now filters to selectable species.

### Takeaways

- The accept rule passed. `fitted` cut the held-out error from 0.1514 to 0.0978.
- Depth 2 reached 99.7 percent of the corpus in four hours on 20 workers.
- Weather and Trick Room carry real signal. Terrain and the status guards are
  dormant in this metagame, not broken.
- A field feature must count what one side gains, not whether the field is up.
  A raw indicator subtracts to zero and reads constant.
- The learning curve stays flat on a real-team corpus, so corpus size is still
  not the limit.
- `MLP_HIDDEN` equals `FEATURE_COUNT`, so a new feature invalidates the shipped
  network file. The `reset` stage of the runbook reseeds it.

## 2026-08-21: Depth-2 cost, and what 500,000 turns buys

- Machine: Windows 11 with 31.5 GB of RAM, 22 pool workers
- Build: release benchmark profile
- Command: `cargo bench --bench depth2_cost`
- Search: double oracle, one damage roll, no critical-hit branches
- Action set: complete. No cap, and no dominance filter.

### The question

The `competitive` preset gives one job 500,000 turn simulations. PIMC must
finish two worlds inside that, so one solve gets 250,000 turns.

### The cost law

A depth-2 solve costs `R + R * K * C`.

- `R` is the root matrix cells that the search evaluated.
- `K` is the chance successors that each root cell kept.
- `C` is the child matrix cells for each successor.

`K` enters one time. The action count enters two times, through `R` and `C`.

### Complete joint actions for each player

| Format | Actions | Matrix cells |
|---|---|---|
| Singles | 10 to 18 | 100 to 180 |
| Doubles | 290 to 722 | 107k to 353k |

Doubles offers about 470 actions for each player. The earlier estimate of 300 in
`benchmarking.rs` is low.

### Measured cost

Turn simulations, averaged over four teamsheet pairings.

| Format | Depth | baseline | policy | policy+cache | vs 250k |
|---|---|---|---|---|---|
| Singles | 2 | 4.4k | 3.9k | 3.9k | 64x under |
| Doubles | 1 | 15.4k | 11.0k | 11.0k | 23x under |
| Doubles | 2 | over 40M | over 40M | over 40M | over 160x over |

`baseline` is index order. `policy` adds `SolveConfig::policy_order`.
`policy+cache` adds a turn cache of 8,192 successor states.

### Findings

1. Singles depth 2 already fits, with 64 times the room it needs. It needed no
   change.
2. Doubles depth 1 fits, with 23 times the room it needs.
3. Doubles depth 2 does not fit. One solve of the cheapest pairing ran past
   40,000,000 turns and 20 minutes. The projection from `R * K * C` with
   `R = C = 11k` is about 121M turns, which agrees.
4. `policy_order` cuts doubles depth 1 by 1.4 times and singles depth 2 by 1.13
   times. It cannot move a value, so this is free.
5. The turn cache saved nothing. It matched `policy` in 23 of 24 rows. A cache
   hit needs one `(position, command, command)` triple two times in one solve.
   The transposition table already answers a repeated position before the turn
   resolution runs, so the triple almost never repeats.
6. `ChanceMode` is a weak lever at one damage roll. Singles depth 2 moved from
   8.2k under `enumerate` to 7.1k under `top4` and 3.1k under `top1`. One roll
   produces few successors, so there is little to discard.

### Why doubles depth 2 cannot fit

A best-response check must read every action one time to prove that the action
is not the best response. One node therefore costs at least `2 * N` cells, and
about 940 cells at `N = 470`. Depth 2 multiplies the cost of two nodes, so
`R * C` has a floor near 884,000 turns. That floor is 3.5 times the 250,000
target, and it assumes one double-oracle round and one cell for each action.

The measured cost is far above the floor. No chance mode and no cache closes
that distance, because both act on `K` and the distance is in `R * C`.

### Parallel workers do not help the budget

A doubles depth-2 solve used 652 CPU seconds over 20 minutes of wall clock, which
is about half of one thread. `CellOracle::batch_limit` prefetches
`(workers + 1) * 2` cells before each best-response check, and the check then
reads cells one at a time so that its bound test can fire.

The budget counts turn simulations, not seconds. More workers therefore lower the
wall clock of a doubles solve and change nothing about whether it fits.

## 2026-08-21: Doubles inside a wall-clock limit

- Command: `cargo bench --bench doubles_wallclock`
- Position: doubles pairing 10x3, the cheapest that `depth2_cost` reports
- Actions: 290 for P1 and 370 for P2, complete
- Limit: 30 seconds

A turn budget and a wall-clock limit ask different questions. A budget counts
turn simulations. A limit counts seconds, so the worker pool matters.

### The prefetch rate

A best-response check reads its cells one at a time, so that its bound test can
abandon an action. Only the prefetch holds work for the pool.

| Workers | Prefetch rate | Turns | Time | Turns for each second |
|---|---|---|---|---|
| 1 | - | 8,266 | 0.90s | 9,200 |
| 22 | 2 | 9,251 | 0.88s | 10,500 |
| 22 | 32 | 14,546 | 0.35s | 42,000 |

Twenty-two workers returned 1.14 times the work of one worker at the old rate.
The pool was idle through most of each check.

The rate must match the depth. A cell of a leaf matrix costs one turn
simulation. A cell of a depth-2 matrix costs a whole depth-1 solve, which is
14,546 turns here. A rate of 32 at depth 2 lowered the depth-2 turn rate from
18.2k for each second to 5.8k, because the check abandoned most of what the
prefetch built. `SearchOracle::prefetch_rate` holds the two rates.

Both the batch limit and the worker request must read that one rate. A request
that divided a deep batch by the leaf rate asked for one worker.

### What 30 seconds reaches

| Search | Result |
|---|---|
| Exact, depth 1 | Finishes in 0.35s |
| Exact, depth 2, top1 | Reaches depth 1 only. 572,089 turns. |
| MCTS, depth 2 | 71,425 iterations. Support 290 of 290, top probability 0.03. |
| MCTS, depth 2, widening | 60,808 iterations. Support 290 of 290, top probability 0.03. |

The sampled search finishes, and its answer is close to uniform. The position
holds 107,300 action pairs, so 71,425 iterations give each pair less than one
visit. The reported error of 0.0008 measures the sampling noise of the value. It
does not measure the quality of the strategy.

### Why exact depth 2 misses the limit

The depth-1 equilibrium uses a support of 5 actions of 290, and 5 of 370.
Pokemon positions have a small support, which is what double oracle exploits.

One depth-2 cell costs 14,546 turns.

| Work | Cells | Turns | Time at 19k turns for each second |
|---|---|---|---|
| The 5x5 support at depth 2 | 25 | 364k | about 19s |
| One best-response scan | 290 | 4.2M | about 221s |
| One complete round | 660 | 9.6M | about 8 min |

A best-response check must read every action one time to prove that the action
is not the best response. One depth-2 round therefore costs about eight minutes,
and convergence takes several rounds.

### The rule this gives

Depth-2 valuation of the depth-1 support fits 30 seconds. A certified depth-2
equilibrium does not, and no rate or cache changes that. The distance is the
scan over every action, and exactness needs that scan.

## 2026-08-21: Budgeted refinement of a doubles position

- Command: `cargo bench --bench doubles_wallclock`
- Position: doubles pairing 10x3, 290 actions against 370
- Search: `solver::refine_seeded_progress_cancellable`, base depth 1, refined
  depth 2, chance mode top1, 22 workers

### What each search returns in 30 seconds

| Search | Value | Strategy |
|---|---|---|
| Exact, depth 1 | 0.3233 | support 5, finishes in 0.35s |
| Exact, depth 2 | 0.3234 | reached depth 1 only |
| MCTS, depth 2 | 0.3694 | support 290 of 290, largest 0.03 |
| Refinement, depth 1 to 2 | 0.3143 | support 3, 0.55 and 0.39 and 0.07 |

The sampled search spreads its iterations over 107,300 action pairs and returns a
strategy that is close to uniform. The refinement spends the same seconds on the
few dozen cells that decide the answer, and it returns a strategy that names an
action.

### How the refinement uses more time

| Limit | Turns | Rounds | Verified P1 | Verified P2 | Value | Support |
|---|---|---|---|---|---|---|
| 10s | 186,896 | 2 | 6 of 290 | 5 of 370 | 0.3150 | 3 |
| 30s | 410,874 | 8 | 9 of 290 | 8 of 370 | 0.3143 | 3 |
| 60s | 903,501 | 18 | 14 of 290 | 13 of 370 | 0.3089 | 4 |

The value still moves between 30 and 60 seconds, so the pass has not converged.
`SolveWarning::ActionsUnverified` reports that, and the answer is not complete.

The pass publishes a strategy after each round, so a caller shows an answer that
improves rather than one answer at the end.

### Takeaways

- Two PIMC worlds of 30 seconds fit one minute, which is the target.
- The pass reaches the exact refined answer when the budget lets it verify every
  action. `a_complete_refinement_equals_the_exact_answer` holds that rule.
- Verified counts stay small. Nine actions of 290 at 30 seconds is the honest
  figure, and the warning carries it.
- Do not read the sampled error of an MCTS answer as strategy quality. The 0.0008
  figure above sits beside a uniform strategy.

## 2026-08-22: The two search families do not share a cost model

- Command: `cargo bench --bench depth1_budget`
- Workers: 22 for the exact sweep, 1 for every sampled row
- Positions: singles pairing 0x1, doubles pairing 10x3

This sweep replaces the preset table. The earlier run sized every preset from
one cost model. There are two, and they disagree on damage rolls and on depth.

### Singles, 18 actions against 18

| Damage rolls | Turns | Nodes | Time | Support | Value |
|---|---|---|---|---|---|
| 1 | 200 | 17 | 0.03s | 4 and 4 | 0.4815 |
| 2 | 306 | 70 | 0.07s | 3 and 3 | 0.4817 |
| 3 | 348 | 91 | 0.14s | 4 and 4 | 0.4816 |
| 4 | 378 | 106 | 0.24s | 4 and 4 | 0.4816 |
| 6 | 414 | 124 | 0.42s | 4 and 4 | 0.4816 |
| 8 | 432 | 133 | 0.66s | 4 and 4 | 0.4816 |
| 16 | 432 | 133 | 0.98s | 4 and 4 | 0.4816 |

### Doubles, 290 actions against 370

| Damage rolls | Turns | Nodes | Time | Support | Value |
|---|---|---|---|---|---|
| 1 | 14,482 | 4,767 | 0.36s | 5 and 5 | 0.3233 |
| 2 | 33,226 | 14,778 | 1.71s | 4 and 4 | 0.3290 |
| 3 | 105,432 | 50,506 | 7.17s | 5 and 5 | 0.3283 |
| 4 | 268,912 | 131,915 | 25.32s | 5 and 5 | 0.3263 |
| 6 | 833,279 | 413,635 | 94.85s | 5 and 5 | 0.3273 |
| 8 | 871,351 | 433,116 | 145.59s | 5 and 5 | 0.3281 |
| 16 | 1,337,061 | 665,719 | 251.92s | 5 and 5 | 0.3273 |

An exact doubles solve at 8 rolls takes 145 seconds. The goal permits 30.

The value moved by 0.006 across the whole sweep. The support held five actions
at every roll count. A high roll count buys no measured accuracy here.

### Where the cost goes

Turn simulations equal matrix cells in every row. One cell costs one turn.

The turn count rises 92 times from one roll to sixteen. The root matrix keeps
its size, because the action counts do not change. The node count rises 140
times instead.

Those extra nodes are forced decisions. A damage roll that faints a Pokemon
opens a replacement node, and `forced_descent` gives that node the remaining
depth rather than one less. At depth 1 the replacement runs a whole depth-1
search of its own, and each of its cells costs another turn simulation.

Two branches that differ only in the health of the survivors reach the same
replacement decision, and each one pays for its own subtree. `TODO.md` item 4
holds the fix.

### An enumerating sampled search: `mcts`

`mcts` resolves a turn with `TransitionMode::Enumerated`. It builds every branch
of the turn and then draws one.

| Damage rolls | Budget | Turns | Iterations | Time | Turns for each second |
|---|---|---|---|---|---|
| 1 | 100,000 | 100,000 | 65,689 | 14.65s | 6,826 |
| 4 | 8,000 | 8,000 | 7,532 | 87.13s | 92 |
| 16 | 600 | 600 | 567 | 277.67s | 2 |

The rate falls 3,400 times between one roll and sixteen. The exact search falls
92 times over the same range.

`mcts` pays the full branch build and then descends into one branch, so the
enumeration is waste. The exact search spends the same build across a whole
matrix cell, so it takes value from it.

A budget of 86,220 turns at 16 rolls is about 12 hours of `mcts`. Give `mcts`
the exact roll count. `BotAlgorithm::enumerates_turn_branches` holds that rule.

### A belief search: `ismcts`

`ismcts` and `mccfr` ignore `MctsConfig::transition`. They always call
`sample_transition`, which draws one outcome without building the rest.

Depth 1, 24 worlds, 100,000 turns of budget:

| Damage rolls | Turns | Iterations | Time | Turns for each second |
|---|---|---|---|---|
| 1 | 100,000 | 55,779 | 28.31s | 3,532 |
| 4 | 100,000 | 56,523 | 32.39s | 3,088 |
| 16 | 100,000 | 56,783 | 45.69s | 2,189 |

Sixteen rolls cost 1.61 times one roll. The same step costs `mcts` 3,400 times.
The two rates are two orders of magnitude apart, and one preset value cannot
serve both.

The iteration count holds near 56,000 in every row. One turn simulation is one
iteration at depth 1, whatever the rolls. The extra time is inside one draw.

A roll is not free here, but keep all sixteen. One roll makes every attack deal
its average damage, so the search cannot tell a roll that faints a target from a
roll that does not. A doubles bot decides on that threshold.

### Depth for a belief search

An exact search multiplies its tree by the branch count of a turn for each ply.
`depth2_cost` measures about eight minutes for one doubles round of depth 2.

A belief search descends one sampled path, so one more ply costs one more draw.

16 rolls, 24 worlds, 100,000 turns of budget:

| Depth | Turns | Iterations | Time | Iterations for each second |
|---|---|---|---|---|
| 1 | 100,000 | 56,783 | 46.23s | 1,228 |
| 2 | 100,000 | 32,415 | 53.36s | 607 |
| 3 | 100,000 | 22,440 | 55.65s | 403 |
| 4 | 100,000 | 17,081 | 56.94s | 300 |

Wall time holds near 50 seconds across the whole range. One turn budget buys the
same seconds at every depth. It buys fewer and deeper iterations instead.

This is what 30 seconds buys:

| Depth | Iterations in 30s | Visits for each root action |
|---|---|---|
| 1 | 36,800 | 127 |
| 2 | 18,200 | 63 |
| 3 | 12,100 | 42 |
| 4 | 9,000 | 31 |

`SAMPLED_PRESET_DEPTH` is 2. Depth 1 makes every number the leaf evaluator
through one turn, and `TODO.md` records that error at about 0.10 of win
probability. Depth 3 leaves 42 visits for each of 290 root actions.

No measurement says whether depth 2 or depth 3 plays better. `TODO.md` item 1
adds the bench that can answer it. Take the depth that keeps more visits until
that bench exists.

### The preset table

| Preset | Rolls, exact | Rolls, sampled | Depth, exact | Depth, sampled | Worlds |
|---|---|---|---|---|---|
| `fast` | 1 | 16 | 1 | 2 | 16 |
| `balanced` | 2 | 16 | 1 | 2 | 24 |
| `competitive` | 3 | 16 | 1 | 2 | 48 |

| Preset | Budget, exact | Budget, sampled | Seconds, belief |
|---|---|---|---|
| `fast` | 57,928 | 9,370 | 5 |
| `balanced` | 132,904 | 56,220 | 30 |
| `competitive` | 421,728 | 224,880 | 120 |

`bot::BELIEF_TURNS_FOR_EACH_SECOND` is 1,874. That is the depth-2 row above.

### Takeaways

- Do not size a fog-of-war budget from an `mcts` measurement. The earlier table
  did that, and it set the wrong clock.
- Measure a belief rate at the depth and the rolls that the preset runs. The
  rate falls with both. A depth-1 rate runs 22 seconds long at depth 2.
- A damage roll costs `ismcts` 1.61 times and `mcts` 3,400 times.
- Depth is linear in wall time for a belief search and exponential for an exact
  one. The two families cannot share one depth.
- The doubles position binds every budget. Singles stays under 450 turn
  simulations across the whole roll sweep.


## 2026-08-23: The evaluator against played games

- Command: `cargo bench --bench eval_calibration -- --teamsheet-mix 1`
- Games: 400, from 200 openings of two games each
- Policy: `policy`, a softmax over `eval::policy_scores` with the fitted policy
  weights
- Format: Champions doubles, 2 active, 4 brought of 6, tera off, mega on
- Wall time: 1.9 s for 5,710 resolved turns

`bin/train_eval` fits `eval::fitted` from labels that `solve` produced, and
`solve` scores its own horizon with those same weights. Its held-out error
therefore measures agreement with that loop. This is the first measurement of
agreement with a game result.

### The self-check

P1 won 200 of 400 games, a rate of 0.500. Each opening plays both sides, so team
strength cancels and this rate belongs near 0.500. Five other seeds gave 0.467,
0.567, 0.483, 0.500, and 0.550 over 60 games each, so the check has real spread.

The position-weighted realized rate of each table is 0.529. That figure weights
each game by its position count, so it is a different quantity from the game
rate above. The gap here is about one standard error of a 400-game sample.

The first version of the driver reported 0.877. P2 read its action draw from a
shifted seed. Every P2 draw therefore sat under 2 to the power of -17, and P2
always took the first action of its list. The self-check alone found this bug.

### The three evaluators, 4,365 positions from 400 games

| Evaluator | MAE | Brier | Log loss | ECE |
|---|---|---|---|---|
| `heuristic` | 0.3716 | 0.1766 | 0.5288 | 0.0581 |
| `fitted` | 0.3334 | 0.1661 | 0.4980 | 0.0370 |
| `fitted_mlp` | 0.3375 | 0.1714 | 0.5113 | 0.0337 |

### The `fitted` curve

| Bucket | Positions | Games | Predicted | Realized | Gap |
|---|---|---|---|---|---|
| 0.0-0.1 | 474 | 138 | 0.052 | 0.040 | 0.012 |
| 0.1-0.2 | 377 | 164 | 0.150 | 0.143 | 0.006 |
| 0.2-0.3 | 376 | 179 | 0.248 | 0.335 | 0.087 |
| 0.3-0.4 | 412 | 200 | 0.350 | 0.340 | 0.010 |
| 0.4-0.5 | 531 | 261 | 0.451 | 0.516 | 0.065 |
| 0.5-0.6 | 541 | 268 | 0.548 | 0.590 | 0.042 |
| 0.6-0.7 | 385 | 199 | 0.646 | 0.681 | 0.034 |
| 0.7-0.8 | 369 | 185 | 0.753 | 0.818 | 0.065 |
| 0.8-0.9 | 426 | 155 | 0.851 | 0.878 | 0.027 |
| 0.9-1.0 | 474 | 133 | 0.951 | 0.928 | 0.022 |

### The `heuristic` curve

| Bucket | Positions | Games | Predicted | Realized | Gap |
|---|---|---|---|---|---|
| 0.0-0.1 | 159 | 81 | 0.069 | 0.044 | 0.025 |
| 0.1-0.2 | 360 | 161 | 0.151 | 0.125 | 0.026 |
| 0.2-0.3 | 494 | 211 | 0.254 | 0.182 | 0.071 |
| 0.3-0.4 | 609 | 252 | 0.351 | 0.397 | 0.046 |
| 0.4-0.5 | 612 | 266 | 0.450 | 0.438 | 0.012 |
| 0.5-0.6 | 625 | 274 | 0.547 | 0.635 | 0.088 |
| 0.6-0.7 | 502 | 232 | 0.651 | 0.749 | 0.098 |
| 0.7-0.8 | 478 | 192 | 0.748 | 0.826 | 0.079 |
| 0.8-0.9 | 338 | 148 | 0.847 | 0.914 | 0.067 |
| 0.9-1.0 | 188 | 74 | 0.936 | 0.963 | 0.027 |

### Run-to-run spread of the default mix

Three runs of `cargo bench --bench eval_calibration`, which draws 20 percent of
its openings from the usage cache:

| Run | `heuristic` Brier | `fitted` Brier | `fitted_mlp` Brier |
|---|---|---|---|
| 1 | 0.1729 | 0.1614 | 0.1653 |
| 2 | 0.1731 | 0.1637 | 0.1677 |
| 3 | 0.1710 | 0.1598 | 0.1643 |

### Takeaways

- `fitted` beats `heuristic` on all four statistics. This is the first evidence
  that the training run improved the evaluator against a game, and not only
  against its own labels.
- `fitted_mlp` sits between the two on MAE, Brier, and log loss. It wins the
  expected calibration error alone. That one statistic ignores sharpness, so it
  cannot carry a model choice by itself.
- The `heuristic` curve is compressed. It never predicts above 0.94, and it
  under-predicts every bucket above 0.5. It does not know how won a won position
  is. The `fitted` curve fills both tails. 474 positions land above 0.9, against
  188 for the hand weights.
- The `fitted` gap column stays under 0.09 everywhere. The largest gap is the
  0.2-0.3 bucket, where the realized rate is 0.335.
- Run-to-run spread of a Brier score is about 0.004 under the default mix. The
  gap between `heuristic` and `fitted` is 0.010 or more, so the ranking survives
  the noise.
- Two weight sets need `--policy hand --teamsheet-mix 1` to play the same games.
  The default policy reads `weights/policy_v1.json`, and a training run rewrites
  that file. The games would then change with the play policy and not with the
  evaluator under test. The table below repeats the measurement under
  `--policy hand`, and the ranking holds.
- A whole 400-game sweep costs 2 seconds under the default policy. The
  measurement is cheap enough to run before and after every training run.

### The accept-rule command, 2,638 positions from 400 games

`cargo bench --bench eval_calibration -- --policy hand --teamsheet-mix 1` plays
a softmax over `eval::HAND_POLICY_WEIGHTS`. No training run moves that constant,
so two runs of this command play the same games.

| Evaluator | MAE | Brier | Log loss | ECE |
|---|---|---|---|---|
| `heuristic` | 0.3918 | 0.1879 | 0.5533 | 0.0475 |
| `fitted` | 0.3590 | 0.1760 | 0.5190 | 0.0257 |
| `fitted_mlp` | 0.3640 | 0.1840 | 0.5386 | 0.0363 |

P1 won 193 of 400 games, a rate of 0.482. `fitted` beats `heuristic` on all four
statistics here as well, so the ranking does not come from the play policy.

A third command, `--policy random --teamsheet-mix 1`, plays uniform joint
actions. `fitted` wins the mean absolute error there and loses the Brier score
and the log loss. Uniform play reaches positions that no policy reaches, so
treat that run as a lower bound and not as the accept rule.

## The bench features, 2026-08-23

`eval::features` now returns 23 values. Three of them read the bench.

| Feature | What it counts |
|---|---|
| `bench_threat` | What the best switch-in does to the opposing actives. |
| `switch_in_damage` | What the opposing actives do to that switch-in. |
| `team_coverage` | The type reach of one living team against the other. |

`eval::best_switch_in` picks one bench Pokemon with a type-chart proxy. The
damage calculation then runs for that Pokemon alone. Bench size does not
multiply the expensive work.

The three columns carry their hand-set values. No training run has moved them
yet. `TODO.md` item 3 owns that run.

### Evaluator cost

`cargo bench --bench solver_speed -- --leaf-cost`, 16 damage rolls, the same
singles position as the run of 2026-08-16.

| Evaluator | One leaf, before | One leaf, now |
|---|---|---|
| `even` | 2 ns | 2 ns |
| legacy weights | 5860 ns | 25097 ns |
| `heuristic` | 5919 ns | 25015 ns |
| `fitted` | 5909 ns | 24804 ns |
| `fitted_mlp` | 6099 ns | 25505 ns |

The leaf costs 4.2 times as much. Two parts explain the rise.

1. The switch-in adds 8 damage calculations for each side. The threat features
   ran 4 on this position, so the damage work is 3 times as large.
2. `team_coverage` costs 5.7 microseconds. A stub that returns zero drops the
   `heuristic` row to 19349 ns.

One turn resolution costs about 50 microseconds on this position, so the leaf
holds a margin of 2. The margin was 8.5 before.

This position is singles, which is the worst case for the ratio. A singles
position holds one active pair, so the threat features run 4 damage
calculations against the 16 that the bench adds. A doubles position holds four
active pairs. The threat features then run 16, and the bench adds 16, so the
damage work grows by a factor of 2 and not 4.

| Evaluator | Depth-2 solve | Turns | Value |
|---|---|---|---|
| legacy weights | 154.44 ms | 1.7k | 0.5642 |
| `fitted` | 346.99 ms | 3.5k | 0.7573 |

### The accept-rule command, 2,638 positions from 400 games

`cargo bench --bench eval_calibration -- --policy hand --teamsheet-mix 1`. The
play policy reads a constant, so this run plays the same 400 games as the run
of 2026-08-22.

| Evaluator | MAE | Brier | Log loss | ECE |
|---|---|---|---|---|
| `heuristic`, before | 0.3918 | 0.1879 | 0.5533 | 0.0475 |
| `heuristic`, now | 0.3889 | 0.1868 | 0.5507 | 0.0480 |
| `fitted`, before | 0.3590 | 0.1760 | 0.5190 | 0.0257 |
| `fitted`, now | 0.3567 | 0.1751 | 0.5167 | 0.0275 |
| `fitted_mlp`, before | 0.3640 | 0.1840 | 0.5386 | 0.0363 |
| `fitted_mlp`, now | 0.3935 | 0.1874 | 0.5526 | 0.0541 |

`fitted` improves the mean absolute error, the Brier score, and the log loss.
The expected calibration error rises by 0.0018. The Brier score decomposes into
a calibration part and a refinement part, and the Brier score falls, so the
change is not a calibration loss.

The run is a no-regression check and not an improvement check. Three untrained
columns carry hand-set weights, so they cannot move the curve on their own.

`fitted_mlp` loses on all four statistics. This is expected. The `reset` stage
reseeded `weights/eval_mlp_v1.json` at width 23, which discards the trained
network and leaves the hand seed. `eval::fitted` is the default evaluator, so
play does not change.

### The entry slot and the move type, 2026-08-23

A switch-in owns no slot. The first version of `bench_features` gave the damage
call the constant slot zero. The call reads the ally of that slot, so a Friend
Guard ally counted in one slot order and not in the other. `eval::entry_slot`
now reads the occupant of each slot, so the choice follows the Pokemon through
an exchange. `solver_tests::slot_order_symmetry` covers the bench columns.

`eval::type_edge` read `MoveData::pokemon_type`. The damage call reads
`effective_move_type`, so a Pixilate Hyper Voice rated neutral against a
Dragon-type defender. `type_edge` now reads the same type.

`cargo bench --bench solver_speed -- --leaf-cost` on the singles position of the
row above. This machine reads a lower floor than that row, so both columns come
from one sitting.

| Evaluator | Before the two fixes | After |
|---|---|---|
| `even` | 2 ns | 2 ns |
| legacy weights | 23255 ns | 23353 ns |
| `heuristic` | 22810 ns | 23488 ns |
| `fitted` | 23560 ns | 23592 ns |
| `fitted_mlp` | 23271 ns | 23652 ns |

Both fixes cost about 3 percent of one leaf at most, which the run-to-run
spread of this bench already covers.

The depth-2 solve of the same position moved from 0.7573 to 0.7561 for
`fitted`, and from 3.5k turns to 3.6k. The move-type fix changes what
`team_coverage` reports, so the value moves with it.

The accept-rule table above holds the run that came after both fixes. The
move-type fix moved `fitted` by 0.0001 on the mean absolute error and by 0.0003
on the expected calibration error.

## 2026-08-23: The rollout label source

`bin/train_eval` gained `--labels rollout`. The source plays whole games with
the search bot on both sides. Every position of one game takes that game's
result as its label, so a label is 1 or 0.

The old `--labels search` source labels a position with `solve` at depth 2.
`solve` scores its own horizon with the committed weights, so that label teaches
the evaluator its own output through one turn. A depth-1 search asks the
evaluator to predict the rest of the game, and a depth-2 value does not hold
that quantity.

### The cost calibration

```sh
./target/release/train_eval.exe --labels rollout --calibrate \
  --calibrate-positions 2000 --workers 20 --seed 7 \
  --teamsheet-dir ../teamsheets/vgcpastes
```

| Figure | Value |
|---|---|
| Openings played | 101 |
| Games played | 202 |
| Games that hit the turn cap | 0 |
| Kept labels | 2,347 |
| Median game | 1.07 s |
| Slowest game | 2.81 s |
| Label rate | 182 labels per second on 20 workers |
| P1 win rate | 0.480 |

A game returns about 11.6 labels and resolves about 15 steps.

The play policy is `TurnPolicy::Search` at 64 iterations and depth 2. One
`mcts::search` answers both sides, because the two sides carry the same
settings.

Each opening plays two games, and the second game exchanges the two sides. The
P1 win rate of 0.482 is the self-check of that pair.

A rollout label costs far less than a `search` label. A depth-2 doubles solve
takes minutes. A whole game takes about one second, and it returns about 13
labels.

### The rate that `--positions` sizes

A `search` run sizes `--positions` by attempted positions. A `rollout` run sizes
it by kept labels. The reported `labels per second` line counts the same thing
that the option counts, so `runbook/refresh_and_train.py` reads it without a
change.

### The held-out split holds whole openings

Every position of one game carries that game's one result. Two positions of one
game are not independent. A sample split would put both sides of one result in
the training set and the held-out set.

The two games of one opening are not independent either. They start from one
drawn position, and the second game exchanges the two sides. `eval::features`
is antisymmetric, so the first recorded position of the second game is the
negated first recorded position of the first game. A split by game would train
on the negated position that it then holds out.

The group of the split is therefore the opening. The `split` line of the report
names the sample count and the opening count of each side.

The run below split by game. The review of this commit found the mirror and
changed the group to the opening. The recorded held-out numbers of that run are
therefore optimistic. Read the split line of the next run for the opening
count.

### The held-out error changes meaning

A 0 or 1 label gives a held-out mean absolute error near 0.42. A depth-2 search
label gives one near 0.10. The two numbers measure different quantities. Do not
compare them across the two label sources.

### The rollout writes no policy file

A rollout holds no root mixture. A one-hot target of the played action would
teach the policy head its own draw. The run leaves `weights/policy_v1.json` at
its committed values, and the report says so.

### Two confounds of this run

1. **The empty-bench convention changed in the same commit.**
   `eval::bench_features` returned 0.0 for `switch_in_damage` when a side had no
   living bench. The weight of that column is negative, so an empty bench read
   the best value of the column. An empty bench now reads
   `NO_SWITCH_IN_DAMAGE` for each living opposing active, which is the worst
   value of the column. The fit and the accept rule both read the corrected
   convention. Neither change can be priced on its own from this run.
2. **The play policy and the accept-rule policy differ.**
   The corpus plays `TurnPolicy::Search`. The accept-rule bench plays a softmax
   over `eval::HAND_POLICY_WEIGHTS`. The fit therefore reads positions that a
   stronger player reached than the positions that the bench scores.

### The two roster pools

The corpus and the accept-rule bench do not read the same rosters.

| Reader | Directory | Rosters |
|---|---|---|
| `bin/train_eval`, through `runbook/refresh_and_train.py` | `teamsheets/vgcpastes` | 758 |
| `benches/eval_calibration`, at its default | `teamsheets` | 14 |

`TeamPool::load` reads the `.txt` files of one directory. It does not descend
into a subdirectory, so `teamsheets` and `teamsheets/vgcpastes` are two disjoint
pools.

The accept rule therefore measures rosters that the fit never read. Pass
`--teamsheet-dir ../teamsheets/vgcpastes` to the bench to measure the pool that
the fit did read.

### The seed rule

`collect_positions`, `play_rollouts`, and `benches/eval_calibration` build an
opening seed with one formula. Seed 1 therefore gives all three the same
openings.

`benches/eval_calibration` is the accept rule. Its default seed is 1. A training
run must use another seed, or the accept rule reads the openings that the fit
already read.

This run used seed 7 for the corpus. The accept-rule bench used its default
seed 1.

The two runs also read two different roster pools, as the section above states.
The seed rule still applies. A reader must be able to check both facts.

### The labeling run

```sh
./target/release/train_eval.exe --labels rollout --positions 350000 \
  --time-budget 1800 --rollout-iterations 64 --rollout-depth 2 \
  --turn-cap 120 --teamsheet-dir ../teamsheets/vgcpastes --workers 20 --seed 7
```

| Figure | Value |
|---|---|
| Openings played | 14,397 |
| Games played | 28,794 |
| Play wall time | 1,801.8 s |
| Steps resolved | 447,779 |
| Games that hit the turn cap | 75 |
| Positions dropped with those games | 8,726 |
| Kept labels | 335,219 from 28,719 games |
| Train split | 268,886 samples from 22,975 games |
| Held-out split | 66,333 samples from 5,744 games |

This run split by game. The group is now the opening, so a rerun reports an
opening count here. Read the section *The held-out split holds whole openings*.

| Model | Train loss | Train MAE | Held-out loss | Held-out MAE |
|---|---|---|---|---|
| `hand` | 0.6167 | 0.4068 | 0.6167 | 0.4066 |
| `fitted` | 0.5944 | 0.4099 | 0.5936 | 0.4098 |
| network | 0.5906 | 0.4092 | 0.5912 | 0.4098 |

The fit lowers the held-out log loss from 0.6167 to 0.5936. It raises the
held-out mean absolute error from 0.4066 to 0.4098. The fit minimizes the log
loss, so the two lines do not disagree.

The network gains 0.0000 mean absolute error against a 0.0020 margin. The run
keeps `eval::fitted`.

### The accept rule, 2,638 positions from 400 games

```sh
cargo bench --bench eval_calibration -- --policy hand --teamsheet-mix 1
```

This is the decider. It reads `teamsheets`, which holds 14 rosters. The corpus
read `teamsheets/vgcpastes`, so this row is out of sample. The bench seed is 1
and the corpus seed is 7.

| Evaluator | Weights | MAE | Brier | Log loss | ECE |
|---|---|---|---|---|---|
| `heuristic` | Hand | 0.3842 | 0.1844 | 0.5442 | 0.0450 |
| `fitted` | Committed | 0.3542 | 0.1751 | 0.5163 | 0.0302 |
| `fitted` | Rollout fit | 0.3944 | 0.1862 | 0.5494 | 0.0617 |
| `fitted_mlp` | Committed | 0.3887 | 0.1850 | 0.5462 | 0.0530 |
| `fitted_mlp` | Rollout fit | 0.4003 | 0.1933 | 0.5674 | 0.0573 |

The rollout fit loses all four statistics. It also loses all four against
`heuristic`, which reads hand weights. The run therefore rejects the fit.
`weights/eval_v1.json` and `weights/eval_mlp_v1.json` keep their committed
values.

### The pool row, informational and in sample

```sh
cargo bench --bench eval_calibration -- --policy hand --teamsheet-mix 1 \
  --teamsheet-dir ../teamsheets/vgcpastes
```

This row reads the 758 rosters that the corpus read. It is in sample, so it
does not decide pass or fail. It answers one question: does the roster pool
explain the loss?

| Evaluator | Weights | MAE | Brier | Log loss | ECE |
|---|---|---|---|---|---|
| `heuristic` | Hand | 0.3905 | 0.1896 | 0.5601 | 0.0412 |
| `fitted` | Committed | 0.3635 | 0.1843 | 0.5455 | 0.0402 |
| `fitted` | Rollout fit | 0.3950 | 0.1847 | 0.5477 | 0.0816 |

The rollout fit also loses on its own rosters. The pool does not explain the
loss. 2,477 positions from 400 games. P1 won 214 of 400.

### The empty-bench fix, priced alone

The recorded row of 2026-08-23 measured the committed weights before the
`switch_in_damage` fix. The row above measures the same weights after it.

| Evaluator | MAE | Brier | Log loss | ECE |
|---|---|---|---|---|
| `heuristic`, before the fix | 0.3889 | 0.1868 | 0.5507 | 0.0480 |
| `heuristic`, after the fix | 0.3842 | 0.1844 | 0.5442 | 0.0450 |
| `fitted`, before the fix | 0.3567 | 0.1751 | 0.5167 | 0.0275 |
| `fitted`, after the fix | 0.3542 | 0.1751 | 0.5163 | 0.0302 |

The fix lowers the mean absolute error and the log loss of both evaluators. It
raises the expected calibration error of `fitted` by 0.0027. The fix ships.
The two rows share one weight vector, so this comparison prices the code change
alone.

### Why the rollout fit lost

The rollout fit is under-confident on the bench positions. Read its buckets:

| Bucket | n | Predicted | Realized | Gap |
|---|---|---|---|---|
| 0.1-0.2 | 161 | 0.150 | 0.012 | 0.137 |
| 0.2-0.3 | 272 | 0.255 | 0.202 | 0.053 |
| 0.7-0.8 | 268 | 0.745 | 0.720 | 0.025 |
| 0.8-0.9 | 165 | 0.849 | 0.970 | 0.121 |
| 0.9-1.0 | 46 | 0.925 | 1.000 | 0.075 |

Both end buckets miss away from 0.5. The fit predicts 0.150 where the games
give 0.012, and it predicts 0.849 where the games give 0.970.

The weight vector shows the same effect. `health` falls from 2.0030 to 1.0183,
and `screens` falls from 0.4728 to 0.1393. A smaller vector pushes every
prediction toward 0.5.

Two facts explain the compression.

1. **The corpus and the bench use two different players.** The corpus plays
   `TurnPolicy::Search` at 64 iterations and depth 2. The bench plays a softmax
   over `eval::HAND_POLICY_WEIGHTS`. A stronger player recovers from a deficit
   more often, so a health edge predicts the result less well in the corpus
   than in the bench games. The fit reads that weaker relation and shrinks the
   weight.
2. **The two kill features are collinear.** The report gives their correlation
   as +0.8971. The fit splits their weight between them, and it gives
   `guaranteed_kill` the value -0.0104.

The pool row rules out the third candidate. The rollout fit loses on the
rosters that it read, so the roster pool is not the cause.

### The learning curve

| Fraction | Samples | Held-out MAE |
|---|---|---|
| 25% | 67,222 | 0.4100 |
| 50% | 134,443 | 0.4098 |
| 75% | 201,665 | 0.4097 |
| 100% | 268,886 | 0.4098 |

Four times the samples moved the error by 0.0002.

`train::subset` takes an evenly spaced stride of the training samples. The
training split holds 22,975 games, and each game holds about 11.7 positions.
A stride of 4 therefore keeps about 3 positions of every game. Every point of
this curve reads the same 22,975 games.

The curve measures positions for each game. It does not measure games. A game
carries one result, so the game is the independent unit. Do not read this curve
as evidence that more games cannot help.

The network line is the evidence that the features are the limit. A hidden
layer over the same 23 features gains 0.0000 held-out mean absolute error. Both
the flat curve and the flat model class point at the feature set.

The mean absolute error also sits at its noise floor. A label is 1 or 0. A
perfect predictor of a position with a true win probability of q still reads an
error of 2q(1-q) there, and the mean predicted value is 0.498. Read the log
loss for the model comparison, and not the mean absolute error.

### The side bias of the self-play corpus

The play stage reports a P1 win rate of 0.469, from 13,476 wins of 28,719
scored games.

Each opening plays two games, and the second game exchanges the two sides. The
rate must therefore sit near 0.500. The standard deviation of the mean is
0.5/sqrt(28719), which is 0.0030. The measured rate is 10.4 standard deviations
below 0.500.

The 75 capped games left their mirror unpaired. Those games can move the rate
by at most 0.0026, so they do not explain the gap.

A game with no winner is dropped and is not scored, so a draw cannot bias the
count.

`TODO.md` item 3b owns this.
