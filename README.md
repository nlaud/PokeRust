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

Fit both from labeled positions:

```sh
cd poke_rust
cargo run --release --bin train_eval -- --positions 400 --label-depth 2 --seed 1
```

The trainer generates teams from the usage cache, plays random legal commands, and records the position before each turn. It then labels each position with an exact solve, and it fits both models by gradient descent.

| Option | Default | Purpose |
|---|---|---|
| `--positions` | `400` | Distinct positions to label |
| `--label-depth` | `2` | Search depth of each label |
| `--seed` | `1` | Seed of the corpus and of every labeled search |
| `--labels` | `search` | `search` solves exactly; `selfplay` samples with the MCTS search |
| `--turns-per-match` | `12` | Turns to play from each generated matchup |
| `--active-per-side` | `1` | Active Pokemon per side |
| `--brought-per-side` | `3` | Team members that each side brings |
| `--holdout` | `0.2` | Fraction of the corpus held out of the fit |
| `--steps` | `400` | Full-batch gradient steps |
| `--learning-rate` | `0.5` | Step size of each descent step |
| `--l2` | `1e-4` | L2 penalty on the weight vector |
| `--meta-root` | `../meta_scraper/data` | The usage cache |
| `--out-eval`, `--out-policy` | `weights/*.json` | Where to write each vector |
| `--dry-run` | False | Report the fit without writing a file |

The trainer needs the usage cache. Run `meta_scraper/update_meta.py` first.

The run prints the training error and the held-out error of the hand-set weights and of the fitted weights. Keep a run only when the fitted weights win on the held-out split.

The labels come from a search that scores its own horizon with the committed weights. One run is therefore one improvement step, not a fixed point. Restore both weight files to `eval::HAND_WEIGHTS` and `eval::HAND_POLICY_WEIGHTS` before a rerun, or the next run starts from the last one. Record each run in `poke_rust/benches/RESULTS.md`.

`cargo test` never runs the trainer. A corpus and its labels cost minutes.

## Read the project documentation

- `frontend/README.md` explains the web interface and tracker grammar.
- `poke_rust/src/information/README.md` explains the fog-of-war inference engine.
- `poke_rust/src/solver/README.md` explains the solver targets, algorithms, and analysis jobs.
- `meta_scraper/README.md` explains the competitive usage-statistics cache.
- `poke_rust/benches/RESULTS.md` contains benchmark results and analysis.
