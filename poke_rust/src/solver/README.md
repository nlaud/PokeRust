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

The server accepts `replacementDepth` from 1 through 8 in a profile request.

## Perfect-information solver

### Team preview

`solver::preview` solves the perfect-information preview.
It runs double oracle over the 180 choices.
It caches or precomputes the matrix cells.
It reports the equilibrium after each completed double-oracle round.
The server can therefore show the newest preview strategy before convergence.

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

`SearchOracle::prefetch_rate` sets the size of a batch.
A cell of a leaf matrix costs one turn simulation.
A cell of an interior matrix costs a whole subtree, which is thousands of turn
simulations in doubles.
The two depths therefore take a different rate.

Both the batch limit and the worker request must read that one rate.
A request that divides a deep batch by the leaf rate asks for one worker.

Only the prefetch holds work for the pool.
A best-response check reads its cells one at a time, so that its bound test can
abandon an action.
A doubles depth-1 solve returned 10.5k turns for each second on 22 workers
against 9.2k turns on one worker before the leaf rate rose.

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

### Policy order

`SolveConfig::policy_order` reads the trained policy at each double-oracle node.
The ranking does two jobs.

1. It orders both best-response checks.
2. Its strongest few actions open the restricted game.

A best-response check abandons an action when the best action so far beats the
optimistic completion of that action.
Index order starts the check from a weak best action.
The check then reads many actions before it can abandon one.
The policy order puts a strong action first.

The order cannot move a value or a strategy.
The check still reads every action.
Double oracle still stops on a best response over the complete action set.
The order changes the cells that a run reads, and therefore the work counters.

`search::policy_seed_actions` sizes the seed.
The seed takes one action for each 64 candidate actions, up to eight.
A restricted game of `s` actions costs `s * s` cells before the first check.
A seed that covers a large part of a small action set rebuilds the whole matrix.
That cost is the work that double oracle exists to avoid.

The root position keeps `RootSeed` instead.
That seed holds the support of the previous deepening pass, which is a measured
answer rather than a prediction.

### Budgeted refinement

`solver::refine_seeded_progress_cancellable` solves at a base depth, then raises
the cells that decide the answer to a deeper one.

A doubles position offers 290 to 722 joint actions for each player.
One depth-2 cell costs a whole depth-1 solve, which is about 14,500 turn
simulations.
One best-response check at depth 2 must read every action, so it costs about 4.2
million turn simulations, and one complete round costs about eight minutes.

The equilibrium support is small.
A measured doubles position played 5 actions of 290, and 5 of 370.
The cells that decide the answer are a few dozen.

The pass runs in three steps.

1. It solves the position exactly at the base depth.
2. It raises the cells of the base support to the refined depth.
3. It admits one more action at a time while the budget lasts.

The candidate order comes from a best-response check at the base depth.
A base cell costs one turn simulation, so that check ranks every action for about
the price of one refined cell.

The order decides which action the pass reads next.
It never removes an action, and it never decides the answer.
An admitted action enters the restricted game with refined cells, and the matrix
solver gives it a probability or does not.

The pass reaches the exact equilibrium of the refined game when it verifies every
action.
`solver_tests::a_complete_refinement_equals_the_exact_answer` holds that rule.
Below that, `SolveWarning::ActionsUnverified` reports the actions that the pass
ranked at the base depth alone.
That warning stops the answer from being complete.

Two rules keep the published answer honest.

A round whose cell fill met a limit never publishes.
A stopped cell takes a static score, a matrix of static scores often ties
everywhere, and the matrix solver then returns a uniform strategy over the whole
restricted game.
The pass keeps its last complete round instead, as a deepening pass keeps its
last complete depth.

The base pass carries a report.
`SearchContext::double_oracle` records its no-round fallback only for a root
call, and it recognizes one by the presence of a report.
Without the report a base pass that completed no round would return the uniform
strategy it started from and never report `NoCompletedRound`.

Measured results are in `benches/RESULTS.md`.
Thirty seconds gave a support of 3 actions with probabilities 0.55, 0.39, and
0.07.
An exact depth-2 search of the same position never left depth 1 in that time, and
a sampled search returned a strategy over all 290 actions with a largest
probability of 0.03.

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

### The determinized baseline

`solver::pimc` is perfect-information Monte Carlo.
It solves each drawn world with the exact search.
It then averages the world strategies at the particle weights.

Do not use this method as the main fog-of-war solver.
Each world solve reads the hidden data of that world.
The mix therefore plays a different action in each world, and no player can do
that.
This defect is strategy fusion.
It makes the value too good for the player that the fog protects.

Every answer carries `SolveWarning::StrategyFusion`.
The profile also names the defect in its approximation list.
The name is part of the method, because this search is a labeled baseline.

`preview::solve_open_list_preview` draws worlds too.
That search averages the payoff matrix and solves one time, so one strategy
covers every world.
It has no strategy fusion.

A stopped world returns an even placeholder instead of a searched value.
The mean of one cell must not hold a placeholder beside searched values.
A cell that a stop reached part way therefore writes nothing to the per-world
table, and the run discards the round that asked for it.

One job budget covers a whole PIMC search.
Each world takes a share through `CancelFlag::child_with_budget`.
Without the share, the first world spends the budget of the job.
The answer would then rest on one world.

An equal share starves every world at once when one solve costs more than that
share.
The answer is then a mixture of positions that static scores answered.
`pimc::first_world_quota` therefore gives the first world the larger of the equal
share and one half of the job budget.
Two worlds can always finish when each one fits in one half.

`pimc::later_world_quota` sizes the rest from what the first world cost.
A world of one belief solves a position of one shape, so that cost predicts the
rest better than an equal split does.
The job budget still bounds the run, and `PimcResult::worlds_solved` reports the
worlds that finished.
Fewer worlds raise the strategy fusion of the answer, and a world that never
searched is worse.

A child flag claims one turn from itself and then from the job above it.
The job can refuse a claim that the share permitted.
A search must therefore read `CancelFlag::simulation_budget_exhausted`, which
walks the chain.
`CancelFlag::simulation_budget_hit` answers for one flag alone.
A search that read the child flag alone would run its whole depth on static
scores and then report the answer as complete.

The search publishes one answer for each world that it finishes.
It sends that answer through the root progress hook, which the exact search
uses for its double-oracle rounds.

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

`analysis.rs` holds the generation, the running job, and the newest checkpoint.
`invalidate` raises the cancel flag of the running job.
The generation check in `accept` drops a result that a state change made old.
`solver::CancelFlag` stops a search that already runs.
A state-change cancellation leaves the old checkpoint in place and marks it stale.
An explicit finish request keeps the newest checkpoint of the current search.

Each job exit writes one console line, and each line names the exit that ran.
The lines hold no count of P2's actions.
P1 starts the server of a hotseat battle, so P1 can read that console.
`accept` returns its line, and the caller writes it after it releases the
session lock.

The server draws the P2 command and returns it as `p2Reveal`.
The client sends no `p2` field for a bot session.
The client waits for the current analysis job until that job stops.
One shared simulation-turn budget stops the full job.
The budget covers all depth passes, preview worlds, and nested searches.
Cached turns do not use the budget.
`POST /api/battles/{id}/analysis` requests the newest result.
The cancel flag stops the active search. The job keeps the newest completed
strategy checkpoint for the draw.
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
`frontend/src/pages/simulate/SetupPanel.tsx` cannot build either pair.
The Settings sidebar holds one search for each information class, and the
information mode of the session picks between them.

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
An exact search uses iterative deepening inside one search. It finishes a lower
depth before it starts a higher depth. All depths use one shared budget.
A sampled search starts at the requested depth. It runs until it uses the
shared simulation-turn budget.
Each rung records its depth.
The panel shows the exact number of claimed simulation turns.
The progress bar compares that count with the shared job budget.

The tracker accepts `ismcts`, `mccfr`, and `pimc` only.
Each of these searches reads the belief instead of treating one hidden world as
the true state.

A position with no lead on either side is the team preview.
The same module then searches the stored team-preview belief with
`solve_open_list_preview`.
It publishes each completed double-oracle strategy and then the final strategy.
Each strategy contains bring-and-lead choices for both players.

`frontend/README.md` explains the two panels that show these answers.

### The streaming job

`solve.rs` runs one job for a battle session or a tracker session, and it
streams each answer over Server-Sent Events.

`POST /api/solve` validates the session and the profile.
It registers one job and returns the job ID.
`GET /api/solve/{id}/events` runs that job one time and streams its answers.
`DELETE /api/solve/{id}` stops the job.

The stream sends `started`, live `progress`, and each `update`.
It then sends `done`, `failed`, or `cancelled`.
Each `update` carries the generation of the position, a revision that rises by
one, the depth, the elapsed time, the value, both complete strategies, and the
search statistics.
A sampled answer also carries its iteration count, its world count, its seed,
and the name of its leaf evaluator.

The job publishes the newest strategy during the search.
An exact double-oracle search publishes after both best-response checks of each
round. MCTS and ISMCTS publish after a completed iteration checkpoint. MCCFR
publishes after a complete pair of player traversals. These checkpoints use
work counts. They do not use elapsed time.
A partial answer carries `complete: false`.

`solver::warnings_are_complete` holds the completion rule of the project.
An answer is complete when no warning says that configured work stopped early.
`solver::sampling_warnings_are_complete` is the same rule for a sampling
search, which treats a spent simulation-turn budget as its terminal limit.
The simulate, tracker, and streaming endpoints all read these two functions.
Each endpoint held its own copy before, and the copies disagreed.

Double oracle publishes an answer only after both best-response checks of a
round.
A stop that arrives before the first round leaves the uniform strategy that the
run started from.
That answer carries `SolveWarning::NoCompletedRound`, because a uniform mixture
is also a real equilibrium of some positions.

`matrix::CellOracle` carries the round hook into the matrix solver.
`solve_seeded_progress_cancellable` supplies that hook at the root position
alone.

A tracker answer carries both strategies.
A battle answer carries the Player 2 strategy and win estimate for each bot
checkpoint. A hotseat battle has no bot profile and carries no bot strategy.
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
- [Search in games with incomplete information](https://www.sciencedirect.com/science/article/pii/S0004370298000116)
- [Why PIMC works](https://ojs.aaai.org/index.php/AAAI/article/view/7562)
- [POMCP](https://proceedings.neurips.cc/paper/2010/hash/edfbe1afcf9246bb0d40eb4d8027d90f-Abstract.html)
- [Online Outcome Sampling](https://www.mlanctot.info/files/papers/aamas15-iioos.pdf)
- [ReBeL](https://arxiv.org/abs/2007.13544)
- [DeepStack](https://dmorrill10.github.io/assets/publications/17science.pdf)
- [Libratus](https://www.ijcai.org/Proceedings/2017/772)
- [Parallel MCTS](https://cris.maastrichtuniversity.nl/en/publications/parallel-monte-carlo-tree-search/)
- [Parallel CFR](https://arxiv.org/abs/2605.14277)
- [Sampled MuZero](https://proceedings.mlr.press/v139/hubert21a.html)
