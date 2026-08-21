# Refresh the metagame and retrain the evaluator

This runbook refreshes the team data and the usage data. It then retrains the
leaf evaluator of the solver.

One Python file does all of the work:

```sh
python runbook/refresh_and_train.py
```

Run the command from the repository root. The default run takes about four
hours. The standard library is the only dependency.

`poke_rust/src/solver/TRAINING.md` holds the manual procedure. This runbook
automates it and adds the team and usage refresh.

## What the script does

The script runs six stages in order.

| Stage | Action | Time |
|---|---|---|
| `pastes` | Reads a VGCPastes export. Fetches each Pokepaste. Writes teamsheets. | 1 minute |
| `meta` | Refreshes the championsbattledata.com usage cache. | 1 minute |
| `reset` | Restores the three weight files. Repairs a stale feature width. | 1 second |
| `build` | Builds `train_eval` in the release profile. | 1 minute |
| `calibrate` | Measures the label cost. Sizes the training run. | 2 minutes |
| `train` | Collects the corpus. Labels it. Fits the weights. | `--hours` |

Each stage writes its own progress lines. A failed stage stops the run.

## Common commands

Run everything with the default four-hour budget:

```sh
python runbook/refresh_and_train.py
```

Run for a different length of time:

```sh
python runbook/refresh_and_train.py --hours 10
```

Skip the network stages when the caches are already current:

```sh
python runbook/refresh_and_train.py --skip pastes --skip meta
```

Run one stage alone:

```sh
python runbook/refresh_and_train.py --only calibrate
```

Report the fit without a change to any weight file:

```sh
python runbook/refresh_and_train.py --dry-run
```

## Stage 1: the teams

The script reads each `.csv` file in `teamsheets/vgcpastes_exports/`. It scans
every cell for a `pokepast.es` link. A spreadsheet export moves its columns
between versions, so a scan is safer than a fixed column index.

The script then fetches `https://pokepast.es/<id>/raw` for each link. That
endpoint returns the team in teamsheet format. `parse_team_sheet_str` reads that
format, so the script needs no conversion step.

Each team goes to `teamsheets/vgcpastes/<id>.txt`. The script skips a file that
is already on disk, so a second run only fetches the new teams.

The script rejects a body that does not look like a team. A deleted paste
returns an error page, and the Rust parser would read that page as an empty
roster.

Pass `--export` to name one export file. Pass `--max-teams` to limit the fetch.

### Why real teams matter

`generate_meta_team` builds a roster from per-Pokemon marginal rates. Those
rates hold the item rate and the move rate of one Pokemon alone. They do not
hold the combinations that a player actually brings.

A real team holds a weather setter beside the Pokemon that use the weather. It
holds a Trick Room setter beside slow attackers. The new field features measure
those relations, so the corpus must contain them.

`--teamsheet-mix` sets the fraction of matchups that use an archived team. The
default is 0.8. The other matchups use the usage cache, which keeps the rare
Pokemon that no archived team brought.

## Stage 2: the usage data

The script runs `meta_scraper/update_meta.py`. That tool writes
`meta_scraper/data/<Season>/<Format>/<slug>.json` and `index.json`.

Read `meta_scraper/README.md` before you change this stage. Three rules matter:

1. `index.json` names the active season. Never use a fixed season name.
2. `stat_points` use the Champions 0 through 32 scale. They are not EVs.
   `build_pokemon_state` converts them. Do not convert them twice.
3. Move percentages are marginal rates. They total about 350, not 100. Do not
   normalize them.

An update replaces the current data. The cache keeps no history.

### A new species name can stop the run

The site adds and renames Pokemon between seasons. An unknown name is an error,
not a silent skip.

The failure looks like this:

```text
cannot read the usage cache at ..\meta_scraper\data:
  species name "Mega Gallade" does not map to a known Species
```

To repair it, add the name to `SPECIES_OVERRIDES` in
`poke_rust/src/meta/names.rs`. Then rebuild.

This command lists every unresolved name at once:

```sh
python runbook/check_species.py
```

## Stage 3: the weight reset

The script restores three files with `git checkout`:

1. `poke_rust/weights/eval_v1.json`
2. `poke_rust/weights/eval_mlp_v1.json`
3. `poke_rust/weights/policy_v1.json`

This step is not optional. The labels come from `solve`, and `solve` scores its
own horizon with the committed weights. Training is one fixed-point step. It
does not converge on its own.

The reset runs before the build. `src/solver/eval.rs` embeds the three files
with `include_str!`, so a build fixes their contents. A build that ran first
would carry the previous weights into every label.

A file from an earlier run makes the accept rule compare the new fit against
that run. The comparison then means nothing.

### The reset can destroy an accepted run

The restore discards whatever the three files hold. An accepted run that nobody
committed lives exactly there.

The stage therefore stops when the files hold uncommitted changes:

```text
! the weight files hold uncommitted changes:
!   M poke_rust/weights/eval_v1.json
! `reset` would discard them, and an accepted run is not recoverable.
```

Commit the weights first. Pass `--force-reset` to discard them on purpose.

### The feature width check

The reset also compares the network file against `FEATURE_NAMES` in
`src/solver/eval.rs`. `MLP_HIDDEN` equals `FEATURE_COUNT`, so a new feature
changes the hidden-layer width. A record from the earlier width cannot be
reshaped, and `MlpRecord::to_network` refuses it.

The script writes the hand-seeded network at the current width when the widths
disagree. The linear file needs no repair. `resolve` fills one name at a time,
so a name that the file does not hold keeps its hand-set value.

## Stage 4: the build

The script runs `cargo build --release --bin train_eval`. The debug profile is
too slow for a labeling run.

## Stage 5: the calibration

The script labels a small sample and measures the cost. It reads two numbers
from the report:

1. The label rate. It multiplies this rate by `--hours` to size `--positions`.
   It adds 15 percent of headroom, so the corpus does not run dry.
2. The slowest label. It sets `--label-deadline` above this value, so a normal
   label is never cut short.

The sample must hold at least three times as many positions as workers. A
sample that fits in one wave measures the slowest label, not the rate.

The script writes the result to `runbook/logs/calibration.json`. A later
`--only train` reads that file. The record holds the settings that change the
label cost. A run that changed one of them measures again.

## Stage 6: the training run

The script collects positions, labels each one, and fits the weights.

The collector plays random legal commands from the paired rosters. A new
mechanic therefore reaches the corpus without a code change.

These label settings make a depth-2 doubles label affordable:

| Setting | Value | Reason |
|---|---|---|
| `--label-depth` | 2 | A depth-3 doubles label costs hours. |
| `--label-chance` | `top1` | A doubles turn returns many successors. |
| `--label-max-actions` | 24 | A doubles side offers hundreds of joint actions. |
| `--iterative-deepening` | on | An expensive position keeps a depth-1 label. |
| `--min-label-depth` | 1 | A label below depth 1 leaves the corpus. |

These settings make the label approximate. An approximate depth-2 label still
searches deeper than the leaf that it teaches. Record the settings in
`poke_rust/benches/RESULTS.md`.

The `--time-budget` stops the labeling stage on the clock. A worker can finish
one active label after the budget expires.

## How to read the report

The report holds six parts.

1. **The depth histogram.** It counts the kept labels at each depth.
2. **The drop list.** It names each reason that a label left the corpus. A large
   count means that the deadline or the depth floor is too strict.
3. **The feature variance.** A feature marked `constant` did not explain any
   difference between samples.
4. **The kill feature correlation.** A value at or above 0.99 means that the two
   kill features are still collinear. Raise `EVAL_DAMAGE_ROLLS`.
5. **The learning curve.** A flat curve means that more positions no longer help
   the linear model.
6. **The model choice.** The network takes the default slot only when it beats
   the linear fit by `--mlp-margin`.

The `tera` feature reads constant under the Champions rules. Both sides report
the same flag, so the difference is always zero. This is correct.

## The accept rule

The script prints the decision at the end of the run:

```text
held-out mean absolute error:  hand 0.0812   fitted 0.0744
ACCEPT: the fit beat the hand weights by 0.0068.
```

Keep a run only when the fitted weights beat the hand-set weights on the
held-out split. A higher error means that the step overshot.

To discard a run, restore the three files:

```sh
cd poke_rust
git checkout -- weights/eval_v1.json weights/eval_mlp_v1.json weights/policy_v1.json
```

Then lower `--learning-rate`, or raise `--l2`, and run again.

## What to commit

Commit these files after an accepted run:

1. `poke_rust/weights/eval_v1.json`
2. `poke_rust/weights/eval_mlp_v1.json`
3. `poke_rust/weights/policy_v1.json`
4. `poke_rust/benches/RESULTS.md`

Commit `src/solver/mod.rs` and `src/solver/mcts.rs` only when the model choice
changes the default evaluator. Commit nothing else from a training run.

Two directories stay out of Git:

- `teamsheets/vgcpastes/` holds fetched pastes. Each paste belongs to its
  author.
- `runbook/logs/` holds the log of each run.

## The field features

This runbook trains a 20-feature evaluator. The last seven features read the
field and the side conditions:

| Feature | What it counts |
|---|---|
| `weather_edge` | What one side gains from the weather that is up now. |
| `weather_control` | Living weather setters, and whether their weather is up. |
| `terrain_edge` | What the grounded Pokemon of one side gain from the terrain. |
| `terrain_control` | Living terrain setters, and whether their terrain is up. |
| `tailwind` | Tailwind on one side, scaled by the turns it has left. |
| `guard_conditions` | Safeguard, Mist, and Lucky Chant on one side. |
| `trick_room` | The remaining Trick Room clock, for the slower side. |

### Why a field feature counts a side, not the field

`features` computes `side_features(P1) - side_features(P2)`. Each feature must
therefore be a P1 quantity minus the matching P2 quantity. A test asserts this
side-swap symmetry.

Weather, terrain, and a pseudo-weather belong to the field. Both sides read the
same value. A raw indicator of the weather holds the same value on both sides.
It subtracts to zero, and the feature reads `constant` in the report.

Each field feature therefore counts what one side gains from the field. It does
not count whether the field is up. `weather_features` and `terrain_features`
hold this rule.

A side condition needs no re-expression. The engine already stores it per side.

### Why Trick Room prices the clock

The `speed` feature already reads the reversed order. A Trick Room feature that
counts the same order would be collinear with it.

The `trick_room` feature therefore prices the remaining turns. The `speed`
feature cannot see the clock, so the two features stay separate.

## A new feature makes one test fail until you retrain

`the_fitted_weights_parse_and_hold_one_value_for_each_feature` reads
`weights/eval_v1.json` directly. It asserts that the file names every entry of
`FEATURE_NAMES`.

The test fails after you add a feature:

```text
the file omits weather_edge
```

This is correct. A name that the file omits keeps its hand-set fallback, and
that fallback would hide a training run that never reached the feature.

The training run writes every name. The test passes again after you accept a run
and commit the weights. Do not repair the test by hand.

## How to add another feature

1. Raise `FEATURE_COUNT` in `poke_rust/src/solver/eval.rs`.
2. Add the name to `FEATURE_NAMES`.
3. Add a starting weight to `HAND_WEIGHTS`.
4. Compute the value in `side_features`.
5. Run this runbook.

The `reset` stage repairs the network file for you. Step 5 needs no other
action.

Write each feature as a P1 quantity minus the matching P2 quantity. A one-sided
feature fails the test suite.

## Attribution

Team data comes from **VGCPastes**. Usage data comes from **Pokemon Champions
Battle Data** (https://championsbattledata.com/). Credit both wherever you use
this data.

This fan project has no affiliation with Pokemon, Nintendo, Game Freak, or
Creatures Inc.
