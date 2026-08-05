# Training the leaf evaluator

`solver::eval` scores a position at the search horizon.
`bin/train_eval` fits the weights of that evaluator from labeled positions.
This document holds the rerun procedure.

Run every command from `poke_rust/`.

## When to rerun

Rerun the training after any of these changes:

1. A new mechanic changes how a position resolves.
2. A new feature joins `FEATURE_NAMES`.
3. A new usage-cache season replaces the team source.
4. A solver change moves the label value.

A run is one improvement step.
The labels come from `solve`, and `solve` scores its own horizon with the
committed weights.
A second run therefore starts from the output of the first run.
The binary does not converge on its own.

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

## The calibration step

Measure the label cost before the long run starts.

```sh
./target/release/train_eval.exe --calibrate --calibrate-positions 60 --workers 20 --seed 1
```

Use at least three times as many positions as workers.
The rate divides the label count by the stage wall time.
A sample that fits in one wave measures the slowest label, not the rate.

The report holds the median, the maximum, and the mean label cost.
It also holds the label rate and the yield of a 1-hour, 10-hour, and 12-hour
budget.

Read two numbers from the report:

1. The label rate. Multiply it by the wanted run time to size `--positions`.
2. The maximum label cost. Set `--label-deadline` above it.

A corpus that runs dry stops the run early.
Set `--positions` about 15 percent above the expected yield.

## The overnight command

```sh
./target/release/train_eval.exe \
  --positions 24000 --label-depth 2 --min-label-depth 1 \
  --label-chance top1 --label-max-actions 24 \
  --time-budget 36000 --workers 20 --seed 1
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

Keep a run only when the fitted weights beat the hand-set weights on the
held-out split.
Compare the `value hand` line against the `value fitted` line.
A lower held-out mean absolute error accepts the run.

Restore the weight files and discard the run when the error rises.

## What to commit

Commit these files:

1. `weights/eval_v1.json`
2. `weights/eval_mlp_v1.json`
3. `weights/policy_v1.json`
4. `benches/RESULTS.md`

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

An old weight file stays readable after step 2.
`resolve` fills one name at a time, and a missing name keeps its hand-set
value.

## How a new mechanic reaches the corpus

The collector plays random legal commands.
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
