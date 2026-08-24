# PokeRust

PokeRust simulates probabilistic Pokémon Champions battles.

## Run the command-line interface

Run this command from `poke_rust/`:

```sh
cargo run -- --p1 ../teamsheets/{teamsheet path} --p2 ../teamsheets/{teamsheet path} -v 3
```

## Run the web interface

1. Start the API server:

   ```sh
   cd poke_rust
   cargo run --release --bin server
   ```

2. Start the frontend server:

   ```sh
   cd frontend
   npm run dev
   ```

3. Open http://localhost:5173.

Read `frontend/README.md` for more information.

## Run the benchmarks

Run the turn-resolution benchmark:

```sh
cd poke_rust
cargo bench --bench turn_speed
```

This benchmark compares enumeration, sampling, damage-roll counts, critical hits, singles, and doubles. It takes a few minutes.

The benchmark skips doubles configurations that require too much memory.

Run the game-tree solver benchmark:

```sh
cd poke_rust
cargo bench --bench solver_speed
```

This benchmark compares three algorithms, search depths, damage-roll counts, and chance-node policies. It takes several minutes.

The benchmark marks an expensive cell as `skipped` and gives a reason.

Each completed cell uses as many teamsheet pairs as its cost budget permits. Therefore, the `pairs` value can differ by row.

Use the `turns` value to compare solver cost. Turn resolution costs much more than equilibrium solving.

Measure the leaf evaluator alone:

```sh
cd poke_rust
cargo bench --bench solver_speed -- --leaf-cost
```

This run skips the sweep. It takes seconds, so a weight change or a feature change is cheap to re-measure.

Record new results in `poke_rust/benches/RESULTS.md` after an engine change.

## Train the solver evaluator

The solver scores a position at its search horizon with a linear model. `poke_rust/weights/eval_v1.json` holds the value weights, and `poke_rust/weights/policy_v1.json` holds the action-policy weights.

Fit the value model from played game results:

```sh
cd poke_rust
cargo run --release --bin train_eval -- --labels rollout --positions 400 --seed 7
```

`--labels` chooses where a value label comes from.

| Source | The corpus | The label |
|---|---|---|
| `rollout` | Whole played games | The result of the game, 1 or 0 |
| `search` | Random legal commands | A depth-2 `solve` |
| `selfplay` | Random legal commands | A sampled search |

A depth-1 search asks the evaluator to predict the rest of the game. Only a game result holds that quantity, so `rollout` is the source that fits the value model. A `search` label scores its own horizon with the committed weights, so it teaches the evaluator its own output through one turn.

A rollout plays each opening twice and exchanges the two sides in the second game. It writes no policy file, because it holds no root mixture.

| Option | Default | Purpose |
|---|---|---|
| `--labels` | `search` | Where the value labels come from |
| `--positions` | `8000` | Positions to collect |
| `--seed` | `7` | Seed of the corpus and of every label |
| `--rollout-iterations` | `64` | Search iterations of each turn of a rollout game |
| `--rollout-depth` | `2` | Search depth of each turn of a rollout game |
| `--turn-cap` | `120` | Steps that one rollout game may take |
| `--label-depth` | `2` | Search depth of each `search` label |
| `--turns-per-match` | `12` | Turns to play from each `search` matchup |
| `--active-per-side` | `2` | Active Pokemon per side |
| `--brought-per-side` | `4` | Team members that each side brings |
| `--holdout` | `0.2` | Fraction of the corpus held out of the fit |
| `--steps` | `400` | Full-batch gradient steps |
| `--learning-rate` | `0.5` | Step size of each descent step |
| `--l2` | `1e-4` | L2 penalty on the weight vector |
| `--meta-root` | `../meta_scraper/data` | The usage cache |
| `--out-eval`, `--out-policy` | `weights/*.json` | Where to write each vector |
| `--dry-run` | False | Report the fit without writing a file |

The trainer refuses an option that its label source ignores. A silent no-op would cost the whole run.

The trainer refuses `--seed 1`. `benches/eval_calibration` is the accept rule of a training run, and it builds its openings from the same formula at that seed.

The trainer needs the usage cache. Run `meta_scraper/update_meta.py` first.

The run prints the training error and the held-out error of the hand-set weights and of the fitted weights. Keep a run only when the fitted weights win on the held-out split, and only when the calibration curve improves.

Restore the weight files to `eval::HAND_WEIGHTS` and `eval::HAND_POLICY_WEIGHTS` before a rerun, or the next run starts from the last one. Record each run in `poke_rust/benches/RESULTS.md`.

`runbook/REFRESH_AND_TRAIN.md` automates the whole procedure. `poke_rust/src/solver/TRAINING.md` holds the manual one.

`cargo test` never runs the trainer. A corpus and its labels cost minutes.

## Read the project documentation

- `frontend/README.md` explains the web interface and tracker grammar.
- `poke_rust/src/information/README.md` explains the fog-of-war inference engine.
- `poke_rust/src/solver/README.md` explains the solver targets, algorithms, and analysis jobs.
- `meta_scraper/README.md` explains the competitive usage-statistics cache.
- `poke_rust/benches/RESULTS.md` contains benchmark results and analysis.
