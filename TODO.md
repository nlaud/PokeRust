Remove items when the work is complete.

Design notes live in the documentation.
`poke_rust/src/solver/README.md` explains the solver targets, the algorithms,
and the analysis jobs.

# New features

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
