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

Record new results in `poke_rust/benches/RESULTS.md` after an engine change.

## Read the project documentation

- `frontend/README.md` explains the web interface and tracker grammar.
- `poke_rust/src/information/README.md` explains the fog-of-war inference engine.
- `meta_scraper/README.md` explains the competitive usage-statistics cache.
- `poke_rust/benches/RESULTS.md` contains benchmark results and analysis.
