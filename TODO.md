Remove items when the work is complete.

Design notes live in the documentation.
`poke_rust/src/solver/README.md` explains the solver targets, the algorithms,
and the analysis jobs.

# Fixes

## Solver Bugs
- [ ] Do all of the following.
  - [ ] We want to support different depth settings for end of turn switches, since they are a simpler decision (No opponent choice here). 
  - [ ] Add a configuration to the solver settings that represents whether the user should be able to see the current found strategy of the opponent, this should also show the full strategy that the move was sampled from.
  - [ ] Update the UI for both surfaces of the solver so that it is not two separate containers it is juts one with a dropdown
  - [ ] Solver settings in tracker should not allow for exact solvers, since that doesnt make sense within context of not having all the information
  - [ ] Simulator bots should not have to think when there is only one possible move -- It should just pick that move
  - [ ] SOlver solutions are generally not better than random guessing especially on high modes, you should investigate this and come up with a solution yourself. Running out of time is currently really bad, especially when you run out on lower depths. We should double the time limit for high effort. 
  - [ ]I think generally we should remove the time cap, support a choose current move that just ends the search early using the current latest found strategy, and use these as time estimates instead. They should still be doubled I think
- [ ] Play several games against the bot using playwright (use the best settings). I want you to evaluate each of its moves and strategies, and understand why some moves it makes are completely nonsensical or understand why it is making each of the moves it makes. Return a file to me about this in depth.

# New features

## Strategy rates in the simulator

- [ ] Show the pick rate of each solver strategy move in the simulator.
  - [ ] Show the rate next to the move, as a percent.
  - [ ] Sort the moves by rate, highest first.
  - [ ] Decide whether the P2 reveal may show the rate of the drawn command.
        `P2Reveal` hides that rate today, because the reveal carries one action
        and nothing else of P2's plan.
  - [ ] Reuse the tracker row format. `TrackerStrategyRow` already carries a
        rate, and `TrackerSolverPanel.tsx` already renders it.

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

## Live analysis

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
