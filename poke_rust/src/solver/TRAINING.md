# Training the leaf evaluator

`solver::eval` scores a position at the search horizon.
`bin/train_eval` fits the weights of that evaluator from labeled positions.
This document holds the manual rerun procedure.

`runbook/REFRESH_AND_TRAIN.md` automates this procedure.
It also refreshes the team data and the usage data first.
Use the runbook for a normal rerun.
Use this document to understand one stage, or to run one stage by hand.

Run every command from `poke_rust/`.

## When to rerun

Rerun the training after any of these changes:

1. A new mechanic changes how a position resolves.
2. A new feature joins `FEATURE_NAMES`.
3. A new usage-cache season replaces the team source.
4. A solver change moves the label value.

A run is one improvement step.

`--labels search` takes each label from `solve`, and `solve` scores its own
horizon with the committed weights.
A second run therefore starts from the output of the first run.
The binary does not converge on its own.

`--labels rollout` takes each label from a played game result, so the label
holds no evaluator output.
The play still reads the committed weights, so a run changes the games that the
next run plays.

## The two label sources

`--labels` chooses the first two stages of the binary.

| Source | The corpus | The label |
|---|---|---|
| `search` | Random legal commands | `solve` at `--label-depth` |
| `selfplay` | Random legal commands | `mcts::search` at `--label-depth` |
| `rollout` | Whole played games | The result of the game, 1 or 0 |

A depth-1 search asks the evaluator to predict the rest of the game.
A depth-2 label does not hold that quantity.
It teaches the evaluator its own output through one turn.
A game result does hold that quantity.
Use `--labels rollout` to fit the value model.

A rollout run reads no search-label option, and a search run reads no rollout
option.
The binary refuses an option that its source ignores.
A silent no-op would cost the whole run.

These options belong to the rollout source alone.

| Option | Purpose |
|---|---|
| `--rollout-iterations` | Search iterations of each turn of a game |
| `--rollout-depth` | Search depth of each turn of a game |
| `--turn-cap` | Steps that one game may take |

One job is one opening, and one opening plays two games.
The second game exchanges the two sides.
The pair removes team strength from the aggregate P1 win rate.
The report prints that rate as a self-check, and it must sit near 0.500.

A game that reaches `--turn-cap` has no winner.
The rollout drops every position of that game.

`--positions` counts kept labels for this source.
The rollout plays openings until it holds that many positions, or until
`--time-budget` expires.

A rollout holds no root mixture, so it writes no policy file.
A one-hot target of the played action would teach the policy head its own draw.
`weights/policy_v1.json` keeps its committed values.

## The rollout split

Every position of one game carries the one result of that game.
Two positions of one game are not independent.

The two games of one opening are not independent either.
They start from one drawn position, and the second game exchanges the two sides.
`eval::features` is antisymmetric.
The first recorded position of the second game is therefore the negated first
recorded position of the first game.

The held-out split holds whole openings for both reasons.
A split by sample, or by game, would train on a position that it then holds out.
The `split` line of the report names the sample count and the opening count of
each side.

## The rollout seed

`collect_positions`, `play_rollouts`, and `benches/eval_calibration` build an
opening seed with one formula.
Seed 1 therefore gives all three the same openings.

`benches/eval_calibration` is test 2 of the accept rule below, and its default
seed is 1.
A training run must use another seed.
The accept rule needs openings that the fit did not read.

`train_eval` refuses `--seed 1`. The default is 7.

Record the corpus seed and the bench seed in `benches/RESULTS.md`.
A later reader must be able to confirm that the two differ.

## The two roster pools

`TeamPool::load` reads the `.txt` files of one directory.
It does not descend into a subdirectory.

`teamsheets` holds 14 rosters, and `teamsheets/vgcpastes` holds 758.
The two pools are disjoint.

`runbook/refresh_and_train.py` gives the corpus `teamsheets/vgcpastes`.
`benches/eval_calibration` reads `teamsheets` at its default.
The accept rule therefore measures rosters that the fit never read.

Pass `--teamsheet-dir ../teamsheets/vgcpastes` to the bench to measure the pool
that the fit did read.
Record the directory of each run in `benches/RESULTS.md`.

## The format contract

The corpus must use Pokemon Champions doubles.
The table below names the value and the file that holds it.

| Setting | Value | Source |
|---|---|---|
| Active Pokemon per side | 2 | `--active-per-side` |
| Roster size | 6 | `--roster-size` |
| Team members brought | 4 | `--brought-per-side` |
| Terastallization | off | `--tera`, absent by default |
| Mega Evolution | on | `--no-mega`, absent by default |
| Stat points | on | `team_preview_state_from_team_strings` |
| Usage table | Doubles | `MetaFormat::from_active_per_side` |

`team_preview_state_from_team_strings` applies `BattleMechanics::default()`.
That default enables Terastallization.
Champions has none, so `start_match` overwrites the rules after it builds the
preview state.
Do not remove that assignment.

The stored Champions formats in `frontend/src/lib/storage.ts` hold the same
values.
Keep both places in agreement.

## Preconditions

1. Refresh the usage cache with `meta_scraper/update_meta.py`.
2. Restore `weights/eval_v1.json`, `weights/eval_mlp_v1.json`, and
   `weights/policy_v1.json` to their hand-set values.
3. Build the release profile with `cargo build --release --bin train_eval`.

Step 2 matters.
The linear fit starts from `HAND_WEIGHTS`.
The network fit starts near the newly fitted linear model.
The accept rule compares both fits on the same held-out split.
A file from an earlier run makes the comparison meaningless.

Run this command to restore the three files:

```sh
git checkout -- weights/eval_v1.json weights/eval_mlp_v1.json weights/policy_v1.json
```

## The cost calibration step

Measure the label cost before the long run starts.
This step measures seconds for each label. It is not the calibration curve of
the accept rule below.

```sh
./target/release/train_eval.exe --labels rollout --calibrate \
  --calibrate-positions 2400 --workers 20 --seed 7 \
  --teamsheet-dir ../teamsheets/vgcpastes
```

The sample must hold at least three waves of jobs.
The rate divides the label count by the stage wall time.
A sample that fits in one wave measures the slowest job, not the rate.

One `search` job is one position, so a search sample needs three times as many
positions as workers.
One `rollout` job is one opening, and one opening yields about 23 labels.
A rollout sample therefore needs about 120 labels for each worker.
`runbook/refresh_and_train.py` sizes both in `calibration_sample`.

The rate counts the same thing that `--positions` counts.
A `search` run sizes `--positions` by attempted positions.
A `rollout` run sizes it by kept labels.

The report holds the median, the maximum, and the mean label cost.
It also holds the label rate and the yield of a 1-hour, 10-hour, and 12-hour
budget.

Read two numbers from the report:

1. The label rate. Multiply it by the wanted run time to size `--positions`.
2. The maximum label cost. Set `--label-deadline` above it.

A corpus that runs dry stops the run early.
Set `--positions` about 15 percent above the expected yield.

## The rollout command

```sh
./target/release/train_eval.exe \
  --labels rollout --positions 350000 --time-budget 1800 \
  --rollout-iterations 64 --rollout-depth 2 --turn-cap 120 \
  --teamsheet-dir ../teamsheets/vgcpastes --workers 20 --seed 7
```

| Option | Reason |
|---|---|
| `--labels rollout` | A game result is the quantity that a depth-1 leaf predicts |
| `--positions` | Sized from the calibrated rate, with headroom |
| `--time-budget` | Stops the play stage on the clock |
| `--rollout-iterations 64` | The bot that plays the games |
| `--rollout-depth 2` | One ply of lookahead for each side |
| `--turn-cap 120` | A doubles game settles well inside this cap |
| `--workers` | Leaves two cores for the machine |
| `--seed 7` | Not the seed of the accept-rule bench |

The play stage holds one feature vector for each recorded position.
It does not hold the position itself.
A run of 350,000 positions therefore costs about 65 MB for the corpus.

## The search command

```sh
./target/release/train_eval.exe \
  --positions 24000 --label-depth 2 --min-label-depth 1 \
  --label-chance top1 --label-max-actions 24 \
  --time-budget 36000 --workers 20 --seed 7
```

Each option has a reason.

| Option | Reason |
|---|---|
| `--positions` | Sized from the calibrated rate, with headroom |
| `--label-depth 2` | An exact depth-3 doubles label costs hours |
| `--min-label-depth 1` | Drops a label that stayed below the wanted depth |
| `--label-chance top1` | A doubles turn has many successors |
| `--label-max-actions 24` | A doubles side offers hundreds of joint actions |
| `--time-budget` | Stops the labeling stage on the clock |
| `--workers` | Leaves two cores for the machine |
| `--seed` | Makes the corpus and every label reproducible |

Add `--teamsheet-dir` to play archived teams instead of generated rosters.
`generate_meta_team` builds a roster from per-Pokemon marginal rates.
Those rates hold no combination, so a generated roster rarely pairs a weather
setter with the Pokemon that use the weather.
The field features measure such pairs, so the corpus needs real teams.

`--teamsheet-mix` sets the fraction of matchups that use an archived team.
The rest use the usage cache, which keeps the rare Pokemon that no archived team
brought.

Add `--iterative-deepening` and `--label-deadline` for a deeper target.
Iterative deepening keeps the last complete pass, so an expensive position
returns a depth-1 label instead of a static score.

`--label-chance` and `--label-max-actions` make the label approximate.
The default dominated-action filter also makes the label approximate.
An approximate depth-2 label still searches deeper than the leaf that it
teaches.
Record the setting in `benches/RESULTS.md`.

The stage budget stops a worker before it starts a new label.
A label that is already active can finish after the budget expires.
Each worker can finish one active label after the budget expires.
Wall-clock limits can also change the label count between runs.

## How to read the report

The report holds six parts.

1. **The depth histogram.** It counts the kept labels at each depth. A run
   with one depth needs no iterative deepening.
2. **The drop list.** It names each reason that a label left the corpus. A
   large drop count means that the deadline or the depth floor is too strict.
3. **The feature variance.** A feature marked `constant` did not explain
   differences between samples. The L2 penalty can still change its weight.
4. **The kill feature correlation.** A value at or above 0.99 means that the
   two kill features are still collinear. Raise `EVAL_DAMAGE_ROLLS`.
5. **The learning curve.** It gives the held-out error of a fit on 25, 50, 75,
   and 100 percent of the training split. A flat curve means that more
   positions no longer help the linear model.
6. **The model choice.** The network takes the default slot only when it beats
   the linear fit by `--mlp-margin`.

The `tera` feature reads constant under the Champions rules.
Both sides report the same flag, so the difference is always zero.
This is correct, and the report marks it.

## The accept rule

A run must pass two tests.

**Test 1. The held-out split.**
Compare the `value hand` line against the `value fitted` line.
A lower held-out mean absolute error passes this test.

This test alone cannot accept a run.

For `--labels search` the reason is the loop.
`solve` produced the labels, and `solve` scores its own horizon with the
committed weights, so the split measures agreement with that loop.
The number moved from 0.0957 to 0.0978 when only the corpus changed.

For `--labels rollout` the reason is the noise.
A label is 1 or 0.
A perfect predictor of an even position still reads an error of 0.5 there.
The held-out error therefore sits near 0.42 and not near 0.10.
Do not compare a rollout number against a search number.
The two measure different quantities.

**Test 2. The calibration curve.**
This test compares a predicted win probability against a played game result.
Run this command from `poke_rust/` before and after the run:

```sh
cargo bench --bench eval_calibration -- --policy hand --teamsheet-mix 1
```

Both flags hold the games still.
`--teamsheet-mix 1` draws every opening from the archived teamsheets.
`--policy hand` plays a softmax over `eval::HAND_POLICY_WEIGHTS`, a constant.

Do not drop `--policy hand` here.
The default policy reads `weights/policy_v1.json`.
A `--labels search` run rewrites that file, so the two reports would then
measure two different sets of games.

Do not drop `--teamsheet-mix 1` either.
The bench uses seed 1, and a training run must use another seed.
Read the section *The rollout seed* above.

Read the `fitted` table. The run passes when its mean absolute error, its Brier
score, and its log loss all fall, and its expected calibration error does not
rise.

Read `src/solver/README.md`, section *Measuring the evaluator*, for the meaning
of each column.

Restore the weight files and discard the run when either test fails.

## What to commit

Commit these files:

1. `weights/eval_v1.json`
2. `weights/eval_mlp_v1.json`
3. `weights/policy_v1.json`
4. `benches/RESULTS.md`

A `--labels rollout` run does not write `weights/policy_v1.json`.
Commit the first two weight files and `benches/RESULTS.md` for that run.

Commit `src/solver/mod.rs` and `src/solver/mcts.rs` only when the model choice
changes the default evaluator.
Commit nothing else from a training run.

## How to add a feature

1. Raise `FEATURE_COUNT` in `src/solver/eval.rs`.
2. Add the name to `FEATURE_NAMES`.
3. Add a starting weight to `HAND_WEIGHTS`.
4. Compute the value in `side_features`.
5. Retrain.

Each feature must be a P1 quantity minus the matching P2 quantity.
That form gives side-swap symmetry for every weight vector.
A test asserts this property, so a one-sided feature fails the test suite.

A feature may read a Pokemon that is not on the field.
`eval::bench_features` reads the bench, and `eval::MoveSelection::Bench` gives
it the move list of an off-field attacker.
A switch clears Disable and a Choice lock, so every move with PP is selectable
there.
An off-field Pokemon owns no slot, so `eval::entry_slot` names the slot that it
would enter.
A slot index is a label, and `slot_order_symmetry` refuses a feature that moves
when the two active slots exchange.
`entry_slot` therefore reads the occupant of each slot and not the index.

An off-field feature costs damage calculations, and a leaf must stay far below
one turn resolution.
Measure it with `cargo bench --bench solver_speed -- --leaf-cost`.

The old linear weight file stays readable after step 2.
`resolve` fills one name at a time, and a missing name keeps its hand-set
value.

The shipped file must still name every feature.
`the_fitted_weights_parse_and_hold_one_value_for_each_feature` reads the file
directly, so a silent fallback fails the test suite.
The `reset` stage appends each missing name at its hand-set value.

The old network file does not stay readable.
`MLP_HIDDEN` equals `FEATURE_COUNT`, so a new feature changes the hidden-layer
width.
`MlpRecord::to_network` refuses a record of the earlier width, and
`fitted_network` then returns `None`.

Write the hand-seeded network at the new width before you retrain.
The `reset` stage of `runbook/refresh_and_train.py` does this for you.

## How a new mechanic reaches the corpus

The collector plays random legal commands, and a rollout plays whole games.
A new mechanic therefore enters the corpus without a change to this binary.

Only a mechanic that needs its own feature needs a code change.
Follow the section *How to add a feature* in that case.

## Troubleshooting

**The corpus is empty.**
The usage cache is absent or holds no team for the format.
Run `meta_scraper/update_meta.py`, then pass `--meta-root`.

**Every label was dropped.**
Read the drop list.
A deadline that is shorter than the median label cost drops every label.
A `--min-label-depth` above the reachable depth does the same.

**A feature reads constant.**
The corpus never changed that quantity.
Check that the rules enable the mechanic that the feature measures.

**A label never finishes.**
Lower `--label-max-actions`, or set `--label-chance top1`.
Add `--label-deadline` with `--iterative-deepening` to keep a shallower label.

**The fit made the evaluator worse.**
Restore the three weight files.
A run is one improvement step, and a step can overshoot.
Lower `--learning-rate`, or raise `--l2`.
