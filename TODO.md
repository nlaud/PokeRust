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
It builds an antisymmetric vector of 20 features.
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

A prediction misses about 10 points of win probability.
A true edge of 55 against 45 is smaller than that error.

## The plan

Run these six items in order.
Item 1 decides whether any later item worked.

- [ ] 1. Measure the evaluator against played games.

      Labels come from `solve` at depth 2.
      `solve` scores its own horizon with the committed weights.
      The fit therefore chases its own output.
      Held-out error measures agreement with a bootstrap, not with truth.

      Add a bench that plays whole games from a fixed opening set.
      Report predicted win probability against realized outcome, in buckets.
      That curve is the acceptance test for items 2 through 6.

      Held-out error moved from 0.0957 to 0.0978 between two training runs.
      Only the corpus changed, from generated rosters to archived teams.
      The number cannot accept or reject a change today.

      This bench is also the only way to accept or reject `SAMPLED_PRESET_DEPTH`.
      It is 2 today. A 30-second answer holds about 36,800 iterations at depth 1,
      18,200 at depth 2, and 12,100 at depth 3. A doubles root offers about 290
      actions, so depth trades lookahead against the visits for each action. No
      measurement says where that trade turns. Run this item before you move that
      constant again.

      Done when: a calibration curve exists, and one command produces it.

- [ ] 2. Let the evaluator read the bench.

      `eval::matchup_features` reads active against active only.
      A benched Pokemon contributes `alive_score` and `status_penalty` alone.
      Its typing, its moves, and its matchup are all invisible.

      Champions doubles brings four Pokemon and leads two.
      Half of each team therefore scores as a health bar.

      Add these features:

      1. The best incoming matchup of the bench against the opposing active.
      2. The damage that the best switch-in takes on entry.
      3. The coverage of the remaining team against the opposing remaining team.

      This is the largest block of information that the position holds and the
      model does not read.

      `MLP_HIDDEN` equals `FEATURE_COUNT`, so a new feature invalidates the
      shipped network. Retrain both fits together.

      Done when: the calibration curve of item 1 improves.

- [ ] 3. Replace the bootstrap labels.

      Depth-2 labels suited a depth-3 search.
      The evaluator only had to rank leaves there.
      A depth-1 search asks the evaluator to predict the rest of the game.

      Label from played-out results with the current bot on both sides.
      The last labeling run held 0.81 labels for each second.
      It produced 11,733 labels in 14,459 seconds against a 4-hour budget.

      One doubles depth-2 label costs minutes, so a rollout is now the cheaper
      source as well as the more honest one.

      Done when: the fit trains on terminal outcomes, and item 1 improves.

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

      Three of the twenty features carry no signal in the current corpus.
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
