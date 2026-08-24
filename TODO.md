Remove items when the work is complete.

Design notes live in the documentation.
`poke_rust/src/solver/README.md` explains the solver targets, the algorithms,
and the analysis jobs.
`poke_rust/src/solver/TRAINING.md` explains how to retrain the evaluator.
`poke_rust/benches/RESULTS.md` holds every measurement that this file cites.

# Root evaluation at depth 1

## Why this is the whole problem now

An exact preset runs one turn of lookahead.
`bot::PRESET_DEPTH` holds that depth.
A belief preset now runs two, because a sampled ply costs one draw.
`bot::SAMPLED_PRESET_DEPTH` holds that depth.

Depth dilutes the evaluator but does not fix it.
A depth-3 sampled path still scores its last state with `eval::fitted`.
The items below stay open for both families.

A depth-1 solve runs five stages.
Only the fourth stage is a guess.

| Stage | Work | Exact |
|---|---|---|
| 1 | Build the legal joint actions of each side | Yes |
| 2 | Grow the double-oracle restricted game | Yes |
| 3 | Resolve one turn for each matrix cell | Yes |
| 4 | Score each outcome branch with the leaf evaluator | No |
| 5 | Average the branches and solve the matrix | Yes |

Stage 4 is `eval::fitted`.
It builds an antisymmetric vector of 23 features.
It takes a dot product with trained weights.
It pushes the result through a logistic function to get a win probability.

A depth-3 search averaged a bad leaf score over two more turns of resolution.
A depth-1 search does not.
Every number that the solver panel shows is the evaluator, through one turn.

## Where the error is

Three measurements point at the feature set.
They also rule out the two obvious fixes.

| Model | Held-out MAE | Reading |
|---|---|---|
| `heuristic` | 0.1514 | Hand-set weights |
| `fitted` | 0.0978 | Ships today |
| `fitted_mlp` | 0.1027 | Same features, one hidden layer |

A network on the same features lost to a dot product by 0.0049.
Four times the corpus moved held-out error by 0.0002.
Both curves are flat because the features hold no more signal.
The model does not lack capacity.

The rollout run of 2026-08-23 repeated both results on 0 or 1 labels.
A hidden layer gained 0.0000 held-out mean absolute error there.
That curve varies the positions of each game and not the game count, so it
does not rule out a larger game count.
Read `benches/RESULTS.md`, section *The learning curve*.

A prediction misses about 10 points of win probability.
A true edge of 55 against 45 is smaller than that error.

## The plan

Run these items in order.
`benches/eval_calibration` decides whether any of them worked.
Read `poke_rust/src/solver/README.md`, section *Measuring the evaluator*.

That bench is also the only way to accept or reject `bot::SAMPLED_PRESET_DEPTH`.
It is 2 today. A 30-second answer holds about 36,800 iterations at depth 1,
18,200 at depth 2, and 12,100 at depth 3. A doubles root offers about 290
actions, so depth trades lookahead against the visits for each action. Measure
that trade with `--policy search` before you move the constant again.

- [ ] 3. Make a rollout fit beat the committed weights.

      `--labels rollout` now ships. It plays whole games with the search bot on
      both sides, and it labels every position with the game result. The split
      holds whole games. The three bench columns train in the same run.
      Read `benches/RESULTS.md`, section *2026-08-23: The rollout label source*.

      The first run lost the accept rule. It lost all four statistics against
      the committed weights, and it lost all four against `heuristic`. The
      weight files keep their committed values.

      The fit is under-confident on the bench positions. `health` fell from
      2.0030 to 1.0183. It predicts 0.150 where the games give 0.012.

      The pool is not the cause. The fit also loses on the 758 rosters that it
      read.

      Two candidates remain. Test them in this order.

      1. The corpus and the bench use two different players. The corpus plays
         `TurnPolicy::Search` at 64 iterations and depth 2. The bench plays a
         softmax over `eval::HAND_POLICY_WEIGHTS`. Measure the fit against
         `--policy search` before you change the corpus.
      2. The two kill features are collinear at +0.8971. The fit gives
         `guaranteed_kill` the value -0.0104. Merge the pair, or add a penalty
         that holds the split still.

      Done when: a rollout fit lowers the mean absolute error, the Brier score,
      and the log loss of test 2, and does not raise its expected calibration
      error by more than 0.005.

- [ ] 3a. Re-time the presets against the 23-feature leaf.

      `bot::PresetLimits` was measured against the 20-feature leaf.
      The bench features raised the singles leaf cost by 4.2 times.
      Read `benches/RESULTS.md`, section *The bench features, 2026-08-23*.

      Arithmetic puts the long belief clock 2 to 3 percent over its budget.
      No preset breaks the 30-second limit on that arithmetic.
      Arithmetic is not a measurement, so this item measures.

      Singles is the worst case for the ratio, and the presets run doubles.
      A doubles position holds four active pairs, so the threat features run 16
      damage calculations against the 16 that the bench adds.
      The doubles rise should be about 2 times and not 4.

      Measure the doubles leaf cost first.
      Re-time only the rows that the measurement moves.

      Do this after item 3.
      The empty-bench convention moves the leaf cost again, and a re-timing run
      costs hours.

      Update `bot::PresetLimits` and
      `frontend/src/components/solver/solverSettings.ts` together.

      Done when: every preset row comes from a doubles measurement of the
      23-feature leaf, and no row exceeds 30 seconds.

- [ ] 3b. Find the side bias of the self-play corpus.

      The rollout play stage reports a P1 win rate of 0.469. It scored 13,476
      wins of 28,719 games.

      Each opening plays two games, and the second game exchanges the two
      sides. The rate must sit near 0.500. The standard deviation of the mean
      is 0.0030, so the measurement is 10.4 standard deviations low.

      The 75 capped games move the rate by at most 0.0026. A game with no
      winner is dropped, so a draw does not bias the count.

      Every rollout label reads this bias. Fix it before the next fit.

      Check these three first.

      1. `play_rollouts` gives one `mcts::search` to both sides. The search
         maximizes the value of P1, so the two sides may not read strategies
         of equal quality.
      2. The seed of the exchanged game may repeat a draw of the first game.
      3. The simulator may hold a real first-player edge. A speed tie and a
         turn order are the two places to look.

      Done when: a paired rollout run of at least 20,000 games reports a P1
      win rate inside 3 standard deviations of 0.500.

- [ ] 3c. Decide what the refine switch does at the shipped depth.

      `frontend/e2e/refine-profile.spec.ts` fails. The switch is visible and
      disabled, so the spec cannot check it.

      `DEFAULT_SOLVER_SETTINGS.depth` reads `PRESET_DEPTH`, which is 1. The
      panel disables the switch at that depth, because a request at the refine
      base depth has nothing to raise.

      Commit 4b31ea7 moved the exact preset to depth 1 and made this state the
      default. The user therefore cannot turn refinement on without a manual
      depth change first.

      Pick one. Hide the switch below the base depth, or let the panel raise
      the depth when the user turns the switch on, or change the spec to
      assert the disabled state.

      Done when: `refine-profile.spec.ts` passes and the panel states the rule
      to the user.

- [ ] 4. Stop the replacement search from multiplying with damage rolls.

      This is a cost defect, and it is why wide damage rolls are expensive.

      A damage roll that faints a Pokemon opens a replacement node.
      `forced_descent` gives that node the remaining depth, not one less.
      At depth 1 the replacement runs a whole depth-1 search of its own.
      Each cell of that search costs another turn simulation.
      A replacement can faint another Pokemon, and `max_forced_chain` is 8.

      More rolls make more branches that faint a Pokemon.
      Each branch opens its own subtree.

      | Damage rolls | Turns | Nodes | Time |
      |---|---|---|---|
      | 1 | 14,532 | 4,792 | 0.33s |
      | 16 | 1,347,463 | 670,920 | 210.83s |

      The root matrix keeps its size across that whole sweep.
      All of the growth is forced decisions.

      Two fixes, in this order:

      1. Merge faint branches that differ only in the health of the survivors.
      2. Score the replacement with a policy head when no depth remains.

      Done when: 16 rolls cost under 5 times 1 roll on the same position.

- [ ] 5. Choose damage rolls by what they change.

      Sixteen rolls cost 93 times one roll and moved the value by 0.004.
      The equilibrium support held five actions at every roll count.
      The outcome average is close to linear in the roll.

      Measured values across the sweep:
      0.3233, 0.3290, 0.3283, 0.3263, 0.3273, 0.3281, 0.3273.
      The spread is 0.0057, and the sequence is not monotonic.

      Read the rolls that cross a faint threshold.
      A roll that moves a health bar by three percent needs no subtree.

      Done when: a chosen roll set matches the 16-roll value inside 0.002.

- [ ] 6. Retire the dead features.

      Three features carry no signal in the current corpus.
      They cost work on every leaf and return a constant.

      | Feature | Variance |
      |---|---|
      | `terrain_control` | 0.0000, constant |
      | `terrain_edge` | 0.0004 |
      | `guard_conditions` | 0.0003 |

      The 758-team corpus explains each number.
      It holds no Grassy, Misty, or Psychic Surge team.
      It holds one Electric Surge team and one Safeguard team.

      Seed the corpus so these features have something to learn from, or remove
      them and take the slots back.

      Do this after item 1, so the removal can be shown to be safe.

      Done when: every shipped feature carries measurable variance.

## Do not do these

The repo already measured each one.

- Do not add model capacity yet.
  `fitted_mlp` lost on the same features by 0.0049.
  A wider model over the same features reads nothing extra.
  Try it again after item 2 adds features.

- Do not train on more positions.
  The learning curve is flat.
  A corpus from 4,800 to 19,200 samples moved held-out error by 0.0002.

- Do not search an exact algorithm deeper by default.
  One doubles depth-2 round costs about eight minutes.
  A certified depth-2 equilibrium needs several rounds.
  `refine` already reaches depth 2 on the cells that decide the answer.
  The Refine switch stays available when a request raises the depth.

  This rule holds for an exact search alone.
  A belief search pays one draw for each ply, so depth is cheap there.
  `bot::SAMPLED_PRESET_DEPTH` is 2 for that reason.

# New features
