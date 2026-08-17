Remove items when the work is complete.

Design notes live in the documentation.
`poke_rust/src/solver/README.md` explains the solver targets, the algorithms,
and the analysis jobs.

# New features

## Parallel search

- [ ] Measure scaling with 1, 2, 4, and more workers.
  - [ ] Use a fixed set of representative doubles positions.
  - [ ] Record time, simulated turns, cache hits, memory, reproducibility, and
        strategy quality.
