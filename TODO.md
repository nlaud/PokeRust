Remove items when the work is complete.

# Fixes: If there is only 1 top-level item below here, should be included in ADDITION to another major TODO item

# New Features
## Solver target

Name these games separately:

1. Perfect-information analysis.
2. Open-list tournament play.
3. Closed-sheet ladder or tracker play.

The tournament list shows every team field except numeric stats.
`OpenTeamSheetNatures` is the closest current information mode.
Keep closed-sheet support, but do not use it as the main tournament target.

Apply the regulation to roster legality, Tera, and Mega Evolution.
Do not search actions that the active regulation forbids.

Official doubles selects four Pokémon and uses the first two as leads.
One six-Pokémon team has 180 bring-and-lead choices.
Two players produce 32,400 preview matrix cells.
Treat team preview as a simultaneous decision.

Use these official time limits:

- Team preview: 90 seconds.
- One move: 45 seconds.
- Player time: 7 minutes.
- One game: 20 minutes.

The interactive solver must return a useful checkpoint well before 45 seconds.
The offline solver can use a longer limit.

The simulator does not model timeout rules.
Label its result as a no-timeout battle win probability.

Tournament matches often use a best-of-three format.
A later match mode must keep learned build data between games.

The solver must support these game properties:

- Simultaneous commands.
- Coupled commands for two active slots.
- Replacement and pivot decisions that do not consume a turn depth.
- Random outcomes.
- Private information.

## Perfect-information solver

### Team preview

`solver::preview` solves the perfect-information case.
It runs double oracle over the 180 choices, and it caches or precomputes cells.

### Approximate search

`solver::mcts` holds the sampling search.
It runs decoupled simultaneous-move MCTS with regret matching or Exp3.
It reports the sampling error and the discarded outcome mass.
`MctsConfig::widening` grows the action set of a node with its visit count.
`solver::exploit` measures a strategy pair against the complete action set.

`MctsConfig::transition` chooses between enumeration and the generative model.
`simulator::generative` holds the generative model.
It samples inside turn resolution, and it returns the trajectory probability and the sampling probability.

- [ ] Test stratified sampling for hits, critical hits, secondary effects, and speed ties.
- [ ] Test common random numbers and control variates.

### Evaluation

- [ ] Calibrate or replace `solver::eval::heuristic`.
  - [ ] Train value and policy models from deeper search or self-play.
  - [ ] Include speed control, damage, KO ranges, Protect pressure, and targeting.
  - [ ] Include field effects, status, boosts, bench resources, Tera, and Mega.
  - [ ] Test calibration, side-swap symmetry, slot-order symmetry, and policy agreement.
  - [ ] Add an evaluator API that supports batches and belief context.

## Fog-of-war solver

Do not average strategies from independent determinized worlds as the main method.
That method lets the solver choose a different action for each hidden world.
Use it only as a labeled perfect-information Monte Carlo baseline.

- [ ] Add ISMCTS as a fast heuristic baseline.
  - [ ] Add outcome-sampling MCCFR as the first equilibrium baseline.
  - [ ] Test public-belief continual solving after the baseline works.
  - [ ] Compare MCCFR with extensive-form double oracle.

  Build these data structures first:
  
  - Weighted beliefs over both players' private data.
  - Normalized particle weights.
  - Posterior updates.
  - Effective sample-size checks.
  - Particle resampling.
  - Observation-based state grouping.
  - Public-belief counterfactual values at depth limits.

  Use `mask_events_for` and the event log as the observation model.
  Do not group hidden worlds only by `MatchState` hash.
  
  - [ ] Add opponent exploitation as a separate mode.
  - [ ] Keep Nash as the default.
  - [ ] Show the exploitability budget for safe exploitation.

## Simulator bot

- [ ] Add an optional P2 solver profile to battle creation.
  - [ ] Support exact and approximate algorithms.
  - [ ] Support time, node, depth, worker, sampling, seed, and action limits (with presets).
  - [ ] Show every approximation and fallback in the interface.

Start P2 analysis after each turn resolves.
Use an immutable state and belief snapshot.
Reuse complete checkpoints until P1 submits a command.

P2 must never read P1's current command.
Sample one P2 command from the latest complete mixed strategy.
Resolve both commands together.

- [ ] Add a generation ID to each analysis job.
  - [ ] Cancel an old job after a state or configuration change.
  - [ ] Ignore results from an old generation.
  - [ ] Keep the last complete checkpoint after failure or cancellation.
  - [ ] Add fast, balanced, strong, and custom bot profiles.
  - [ ] Show private progress without showing the live P2 strategy.
  - [ ] Store deterministic replay data.
  - [ ] Show the sampled P2 action only after both commands lock.

## Parallel search

- [ ] Use a bounded CPU pool that is separate from Tokio and benchmark workers.
  - [ ] Keep the matrix solver and double-oracle control loop serial.
  - [ ] Parallelize missing matrix cells and full best-response checks.
  - [ ] Give each worker a local RNG, statistics set, and cache.
  - [ ] Derive RNG seeds from stable job identifiers.
  - [ ] Merge results in a stable order.
  - [ ] Bound in-flight work.
  - [ ] Add cancellation checks between cells, successors, and nodes.

Do not calculate a full matrix only to fill all CPU cores.
Do not prioritize parallel serialized alpha-beta.
Current benchmarks show that its extra turn simulations cost more than its cell savings.

Measure scaling with 1, 2, 4, and more workers.
Use a fixed set of representative doubles positions.
Record time, simulated turns, cache hits, memory, reproducibility, and strategy quality.

## Live analysis

- [ ] Add `POST /api/solve`.
  - [ ] Add an SSE stream at `/api/solve/{id}/events`.
  - [ ] Stream `started`, `update`, `done`, `failed`, and `cancelled`.
  - [ ] Include a stable generation and revision in each update.
  - [ ] Include depth, time, value, strategy, and search statistics.
  - [ ] Include sampling and model details for approximate results.

  Publish a result only after a complete depth.
  During double oracle, publish after both full best-response checks.
  
  For exact cells, show the certified value interval.
  For sampled cells, show an empirical search gap and confidence information.
  Do not label a partial matrix as an equilibrium.
  
  Show the full mixed strategy.
  Keep the last complete result visible while the next depth runs.
  Show value stability and support changes.
  Limit progress messages to a readable rate.
  Do not submit a suggested command automatically.

## Research

- [Pokémon VGC Tournament Handbook](https://mcdn.pokemon.com/pokemon-prod/raw/upload/v1/live/static-assets/content-assets/cms2/pdf/play-pokemon/rules/play-pokemon-vgc-tournament-handbook-en.pdf)
- [Pokémon Champions gameplay](https://champions.pokemon.com/en-us/gameplay/)
- [Simultaneous-move search](https://www.sciencedirect.com/science/article/pii/S0004370216300285)
- [SM-MCTS convergence](https://arxiv.org/abs/1804.09045)
- [Monte Carlo star-minimax](https://www.ijcai.org/Proceedings/13/Papers/093.pdf)
- [Sparse sampling](https://www.ijcai.org/Proceedings/99-2/Papers/093.pdf)
- [Online Double Oracle](https://eprints.soton.ac.uk/471822/2/online_double_oracle.pdf)
- [Anytime Double Oracle](https://openreview.net/pdf?id=J2TZgj3Tac)
- [Regret-Minimizing Double Oracle](https://proceedings.mlr.press/v202/tang23b.html)
- [Extensive-form Double Oracle](https://www.cs.utep.edu/kiekintveld/papers/2013/bklcp-DO.htm)
- [XDO](https://arxiv.org/abs/2103.06426)
- [Double Oracle lower bounds](https://www.ijcai.org/proceedings/2024/0336.pdf)
- [ISMCTS](https://orangehelicopter.com/academic/papers/tciaig_ismcts.pdf)
- [POMCP](https://proceedings.neurips.cc/paper/2010/hash/edfbe1afcf9246bb0d40eb4d8027d90f-Abstract.html)
- [Online Outcome Sampling](https://www.mlanctot.info/files/papers/aamas15-iioos.pdf)
- [ReBeL](https://arxiv.org/abs/2007.13544)
- [DeepStack](https://dmorrill10.github.io/assets/publications/17science.pdf)
- [Libratus](https://www.ijcai.org/Proceedings/2017/772)
- [Parallel MCTS](https://cris.maastrichtuniversity.nl/en/publications/parallel-monte-carlo-tree-search/)
- [Parallel CFR](https://arxiv.org/abs/2605.14277)
- [Sampled MuZero](https://proceedings.mlr.press/v139/hubert21a.html)
