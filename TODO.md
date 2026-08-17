Remove items when the work is complete.

Design notes live in the documentation.
`poke_rust/src/solver/README.md` explains the solver targets, the algorithms,
and the analysis jobs.

# New features

## Add solver strategy info to frontend
- [ ] Add `POST /api/solve`.
  - [ ] Add an SSE stream at `/api/solve/{id}/events`.
  - [ ] Stream `started`, `update`, `done`, `failed`, and `cancelled`.
  - [ ] Include a stable generation and revision in each update.
  - [ ] Include depth, time, value, strategy, and search statistics.
  - [ ] Include sampling and model details for approximate results.
  - [ ] Publish a result only after a complete depth.
  - [ ] Publish after both full best-response checks during double oracle.
  - [ ] Limit progress messages to a readable rate.
  - [ ] Report the certainty of each result.
    - [ ] Show the certified value interval for exact cells.
    - [ ] Show an empirical search gap and confidence data for sampled cells.
    - [ ] Do not label a partial matrix as an equilibrium.
  - [ ] Show the result in the client.
    - [ ] Show the full mixed strategy.
    - [ ] Keep the last complete result visible while the next depth runs.
    - [ ] Show value stability and support changes.
    - [ ] Do not submit a suggested command automatically.

## Parallel search

- [ ] Use a bounded CPU pool that is separate from Tokio and benchmark workers.
  - [ ] Keep the matrix solver and double-oracle control loop serial.
  - [ ] Parallelize missing matrix cells and full best-response checks.
  - [ ] Give each worker a local RNG, statistics set, and cache.
  - [ ] Derive RNG seeds from stable job identifiers.
  - [ ] Merge results in a stable order.
  - [ ] Bound in-flight work.
  - [ ] Add cancellation checks between cells, successors, and nodes.
  - [ ] Do not calculate a full matrix only to fill all CPU cores.
  - [ ] Do not prioritize parallel serialized alpha-beta. Current benchmarks
        show that its extra turn simulations cost more than its cell savings.
- [ ] Measure scaling with 1, 2, 4, and more workers.
  - [ ] Use a fixed set of representative doubles positions.
  - [ ] Record time, simulated turns, cache hits, memory, reproducibility, and
        strategy quality.
