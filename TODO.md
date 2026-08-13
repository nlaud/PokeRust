Remove items when the work is complete.

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

`simulator::stratify` holds the Latin hypercube plan.
`generative::sample_transition_batch` draws a stratified batch of successors.
One batch member keeps the law of one independent sample.
`TransitionMode::Generative` carries the batch size, and each chance node spreads one batch over consecutive visits.

`MctsConfig::common_random_numbers` gives each node a pool of universe seeds.
Each action pair uses the same seed for the same resolution index.
`MctsConfig::control_variate` subtracts the running mean reward of an action before the learner divides by its selection probability.
Both controls lower the exploitability gap of the learned strategy.
Neither lowers the error of the reported value.

## Fog-of-war solver

Do not average strategies from independent determinized worlds as the main method.
That method lets the solver choose a different action for each hidden world.
Use it only as a labeled perfect-information Monte Carlo baseline.

`solver::belief` holds the particle filter.
It has weighted particles, normalized weights, posterior updates,
effective sample-size checks, resampling, and the observation key.
`solver::ismcts` is the fast heuristic baseline that reads it.
`solver::mccfr` is the outcome-sampling equilibrium baseline.
It also reports the counterfactual value of each public belief at the depth limit.

`mccfr::search_with_leaves` reads supplied information-set values at the depth limit.
`MccfrConfig::horizon_worlds` keeps the worlds of each public belief at that limit.
`mccfr::continual_solve` retains private histories and solves each public belief.
It then solves the root against the information-set values.

`solver::exploit::respond` answers an opponent model as a restricted Nash response.
The opponent plays the model with the supplied confidence, and plays freely with the rest of the mass.
A confidence of zero returns the Nash strategy, and a confidence of one returns a pure best response.
`ResponseReport::budget_spent` reports the worst-case loss against the Nash value.
`exploit::respond_within_budget` scans a confidence ladder and holds that loss under a limit.
Nash stays the default of every search.

## Simulator bot

`analysis.rs` holds the generation, the running job, and the last checkpoint.
`invalidate` raises the cancel flag of the running job, and the generation check
in `accept` drops a result that a state change has already made old.
`solver::CancelFlag` stops a search that already runs, so a cancelled job leaves
the last complete checkpoint in place.

The server draws P2's command and returns it as `p2Reveal`. The client sends no
`p2` field for a bot session. It waits for the current analysis job until that
job stops, and no client timeout ends the wait. `P2RevealPanel` shows the wait
line, the elapsed time, and a "Change my move" button during the search, and it
shows the drawn command after the turn resolves. A job that ends with no answer
blocks one submission and reports the reason. The next submission plays the
turn.

The wait does not remove the uniform draw. `draw_p2_command` also drops a
checkpoint whose strategy read data that the fog of war hides, which an exact
or `mcts` profile does in every fog-of-war battle.

- [ ] Make the default simulate profile play its own search. The setup panel
      defaults to `doubleOracle`, and the default information mode hides data,
      so `strategy_respects_fog` rejects every answer and P2 draws at random on
      every turn.
  - [ ] Default the P2 algorithm to a belief search, or warn in the picker when
        the chosen algorithm cannot play under the selected information mode.

- [ ] Searching in tracker mode should be from player 1's perspective. This means that leads should have the back pokemon for player 1 and that information should be used for the tracking. 

`simulator::scoped_abort_signal` carries the deadline and the cancel flag into one
turn simulation. The hit loop, the target loop, and the action queue read it. The
hit loop and the target loop also merge their equal branches after each step, so
an exact five-hit move keeps a small branch set. `search::resolve` installs the
signal, and a cell whose simulation stops takes a static score.

`tracker_analysis.rs` runs the same profile for a tracker session. It draws one
world from the belief, then runs one search for each depth from one through the
configured depth. Each depth publishes a complete answer, so the panel moves
while the search goes deeper. Each rung also records its depth and its time
budget, so the panel shows an approximate progress figure between two answers.
`TrackerSolverPanel` shows the win odds and the best strategy of both players
below the event input instructions.

A position with no lead on either side is the team preview. The same module
then searches the stored team-preview belief with `solve_open_list_preview`,
and it publishes one rung of bring-and-lead choices.

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
