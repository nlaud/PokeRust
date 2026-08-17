# Solver

The solver computes a mixed Nash strategy for one position.
It models each turn as a simultaneous stochastic game.
`CLAUDE.md` holds the invariants that every change must keep.
This file explains the search targets, the algorithms, and the analysis jobs.

## Search targets

The project names three games separately:

1. Perfect-information analysis.
2. Open-list tournament play.
3. Closed-sheet ladder or tracker play.

The tournament list shows every team field except numeric stats.
`OpenTeamSheetNatures` is the closest current information mode.
The project keeps closed-sheet support.
Closed-sheet play is not the main tournament target.

The solver must support these game properties:

- Simultaneous commands.
- Coupled commands for two active slots.
- Replacement and pivot decisions that do not consume a turn depth.
- Random outcomes.
- Private information.

### Regulation limits

Apply the regulation to roster legality, Tera, and Mega Evolution.
Do not search an action that the active regulation forbids.

Official doubles selects four Pokemon and uses the first two as leads.
One six-Pokemon team has 180 bring-and-lead choices.
Two players produce 32,400 preview matrix cells.
Team preview is a simultaneous decision.

The simulator does not model timeout rules.
Its result is a no-timeout battle win probability.

Tournament matches often use a best-of-three format.
A later match mode must keep learned build data between games.

## Forced decisions

A replacement and a self-switch pivot resolve inside the turn that starts them.
Such a decision consumes no turn depth.
`max_forced_chain` bounds the length of one chain of such decisions.
`solver::forced_descent` holds this rule, and all four searches call it.

`SolveConfig::replacement_depth` and `MctsConfig::replacement_depth` give a
forced decision its own depth.
`None` gives the forced child the remaining turn budget, as a turn gets.
`Some(value)` gives the forced child `value` turns of lookahead.
The value clamps to a minimum of 1, because depth 0 makes no decision at all.
A search that starts at a forced decision also uses this depth.

A value below the remaining depth makes a replacement cheaper to search.
A value above the remaining depth searches past the turn budget of the root.
The second case extends the horizon, so one path extends one time.
After that, a forced child takes the lower of the value and the remaining depth.
This bound makes every path finite.

The counter that each search passes to its cache holds the extension flag in its
high bit.
The node state therefore keeps its size, and a cached value cannot cross an
extension boundary.

No bot preset sets this field.
The server accepts `replacementDepth` from 1 through 8 in a profile request.

## Perfect-information solver

### Team preview

`solver::preview` solves the perfect-information preview.
It runs double oracle over the 180 choices.
It caches or precomputes the matrix cells.

### Parallel search

`SolveConfig::workers` asks double oracle for more than one worker.
A value of 0 or 1 keeps the serial search.

`solver::pool` holds the permits of the process.
The capacity comes from `std::thread::available_parallelism`.
`WorkerPool::acquire` never blocks, and it can return no permit.
A batch with no permit runs on the calling thread alone.

The pool uses neither the Tokio runtime nor the benchmark threads.
Every concurrent solve draws from the one permit count.
The calling thread takes no permit, so a batch with `n` permits uses `n + 1`
threads.

Only the root position makes a batch.
One solve therefore has one level of parallelism.
The matrix solver and the double-oracle control loop stay serial.
`SearchContext::serial_ab` also stays serial.

The double-oracle round uses a batch in two places:

1. The fill of the missing restricted cells.
2. A prefetch before each best-response check.

`CellOracle::batch_limit` bounds the prefetch.
Without the bound the prefetch would fill the whole matrix.
That is the work that double oracle exists to avoid.

A parallel solve returns the value and the strategies of a serial solve.
Three facts give this property:

1. A cell value is exact.
   A cache changes the speed of a cell, not its value.
2. A job seed comes from the identity of the job, not from the worker.
3. A prefetched cell only adds a known value.
   The check abandons a row only when even its optimistic bound loses.
   Such a row is never the best response.

The pool runs only under an exact chance mode.
`ChanceMode::Sample` draws from the random generator.
A sampled cell value also depends on the caches of its worker.
A sampling solve therefore keeps the serial path.

The cost counters do depend on the thread schedule.
A cache hit depends on the job order of one worker.
A solve that hits the node budget or the deadline depends on the schedule for
the same reason.

One worker holds its own transposition table.
A large `tt_capacity` therefore multiplies the memory of a solve.
Set `VERBOSITY` to zero before a large search, because two workers interleave
their output.

### Approximate search

`solver::mcts` holds the sampling search.
It runs decoupled simultaneous-move MCTS with regret matching or Exp3.
It reports the sampling error and the discarded outcome mass.
`MctsConfig::widening` grows the action set of a node with its visit count.
`solver::exploit` measures a strategy pair against the complete action set.

### Generative transitions

`MctsConfig::transition` chooses between enumeration and the generative model.
`simulator::generative` holds the generative model.
It samples inside turn resolution.
It returns the trajectory probability and the sampling probability.

`simulator::stratify` holds the Latin hypercube plan.
`generative::sample_transition_batch` draws a stratified batch of successors.
One batch member keeps the law of one independent sample.
`TransitionMode::Generative` carries the batch size.
Each chance node spreads one batch over consecutive visits.

### Variance controls

`MctsConfig::common_random_numbers` gives each node a pool of universe seeds.
Each action pair uses the same seed for the same resolution index.
`MctsConfig::control_variate` subtracts the running mean reward of an action.
The learner then divides by the selection probability.

Both controls lower the exploitability gap of the learned strategy.
Neither control lowers the error of the reported value.

## Fog-of-war solver

Do not average strategies from independent determinized worlds as the main
method.
That method lets the solver choose a different action for each hidden world.
Use it only as a labeled perfect-information Monte Carlo baseline.

`solver::belief` holds the particle filter.
It has weighted particles, normalized weights, posterior updates,
effective sample-size checks, resampling, and the observation key.
`solver::ismcts` is the fast heuristic baseline that reads the filter.
`solver::mccfr` is the outcome-sampling equilibrium baseline.
It also reports the counterfactual value of each public belief at the depth
limit.

`mccfr::search_with_leaves` reads supplied information-set values at the depth
limit.
`MccfrConfig::horizon_worlds` keeps the worlds of each public belief at that
limit.
`mccfr::continual_solve` retains private histories and solves each public
belief.
It then solves the root against the information-set values.

### Opponent response

`solver::exploit::respond` answers an opponent model as a restricted Nash
response.
The opponent plays the model with the supplied confidence.
The opponent plays freely with the rest of the mass.
A confidence of zero returns the Nash strategy.
A confidence of one returns a pure best response.

`ResponseReport::budget_spent` reports the worst-case loss against the Nash
value.
`exploit::respond_within_budget` scans a confidence ladder.
It holds that loss under a limit.
Nash stays the default of every search.

## Analysis jobs

The server runs the solver as a background job.
These files live in `poke_rust/src/bin/server/`.

### Simulator bot

`analysis.rs` holds the generation, the running job, and the last checkpoint.
`invalidate` raises the cancel flag of the running job.
The generation check in `accept` drops a result that a state change made old.
`solver::CancelFlag` stops a search that already runs.
A state-change cancellation leaves the last complete checkpoint in place.
An explicit finish request keeps the cancelled search result.

Each job exit writes one console line, and each line names the exit that ran.
The lines hold no count of P2's actions.
P1 starts the server of a hotseat battle, so P1 can read that console.
`accept` returns its line, and the caller writes it after it releases the
session lock.

The server draws the P2 command and returns it as `p2Reveal`.
The client sends no `p2` field for a bot session.
The client waits for the current analysis job until that job stops.
No client timeout ends the wait.
A profile time is an expected duration. It does not stop the solver.
`POST /api/battles/{id}/analysis` requests the current complete result.
The cancel flag stops the active search, and the job keeps its last complete
strategy for the draw.
A job that ends with no answer blocks one submission and reports the reason.
The next submission plays the turn.

Before a draw, `draw_p2_command` removes strategy rows that are not legal in
the current state. It normalizes the remaining positive weights. This rule is
important during end-of-turn replacement decisions, where an old or malformed
row must not force a uniform draw.
If P2 has exactly one legal joint action, the server publishes it immediately
and does not start a search.

`draw_p2_command` drops a checkpoint whose strategy read hidden data.
An exact or `mcts` profile reads hidden data in every fog-of-war battle.
`create_battle` now refuses that pair, so the drop is a second guard.
No battle session reaches it today.

`create_battle` refuses a profile that cannot control P2 in the session.
`bot_algorithm_fits_mode` in `routes.rs` holds the rule.
A belief search needs a fog-of-war mode, because Perfect Information builds no
belief.
An exact or `mcts` profile needs Perfect Information.
Both other pairs give P2 a uniform draw on every turn, so the endpoint returns
422.
`frontend/src/pages/simulate/SetupPanel.tsx` disables the same pairs.

`botP2.algorithm` is optional.
A request that names no algorithm takes the search of its information mode.
`default_bot_algorithm` in `routes.rs` makes that choice.
The response reports the resolved name in `botP2.algorithm`.

### Search cancellation

`simulator::scoped_abort_signal` carries a cancel flag into one turn simulation.
The hit loop, the target loop, and the action queue read the signal.
The hit loop and the target loop also merge their equal branches after each
step.
An exact five-hit move therefore keeps a small branch set.
`search::resolve` installs the signal.
A cell whose simulation stops takes a static score.

### Tracker panel

`tracker_analysis.rs` runs the same profile for a tracker session.
It draws one world from the belief.
It then runs one search for each depth from one through the configured depth.
Each depth publishes a complete answer, so the panel moves while the search
goes deeper.
Each rung records its depth and expected duration.
The panel shows an approximate progress figure between two answers.
The figure stays below 100 percent until the server publishes an answer.

The tracker accepts `ismcts` and `mccfr` only.
Both algorithms search the belief instead of treating one hidden world as the
true state.

A position with no lead on either side is the team preview.
The same module then searches the stored team-preview belief with
`solve_open_list_preview`.
It publishes one rung of bring-and-lead choices.

`frontend/README.md` explains the two panels that show these answers.

### The streaming job

`solve.rs` runs one job for a battle session or a tracker session, and it
streams each answer over Server-Sent Events.

`POST /api/solve` validates the session and the profile.
It registers one job and returns the job ID.
`GET /api/solve/{id}/events` runs that job one time and streams its answers.
`DELETE /api/solve/{id}` stops the job.

The stream sends `started`, then each `update`, and then one of `done`,
`failed`, or `cancelled`.
Each `update` carries the generation of the position, a revision that rises by
one, the depth, the elapsed time, the value, both complete strategies, and the
search statistics.
A sampled answer also carries its iteration count, its world count, its seed,
and the name of its leaf evaluator.

The job publishes one answer at the end of each depth.
An exact double-oracle search also publishes one answer after both
best-response checks of each round.
A round answer carries `complete: false`, and a rate limit holds those answers
to one each 250 milliseconds.
A depth answer ignores that limit.

`matrix::CellOracle` carries the round hook into the matrix solver.
`solve_seeded_progress_cancellable` supplies that hook at the root position
alone.

A tracker answer carries both strategies.
A battle answer carries the Player 2 strategy only when the profile holds
`revealStrategy`.
A committed turn cancels every job of that session.

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
