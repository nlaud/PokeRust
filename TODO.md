Remove items when the work is complete.

Design notes live in the documentation.
`poke_rust/src/solver/README.md` explains the solver targets, the algorithms,
and the analysis jobs.

# Root evaluation at depth 1

Every server preset now runs one turn of lookahead.
The leaf evaluator therefore decides the whole answer.
`benches/RESULTS.md` holds every measurement that these items cite.

The two questions to answer, in the order that the evidence supports:

1. What information can the evaluator read? The features are the constraint.
2. Can the architecture be better? Not yet. A network on the same features
   already lost by 0.0049 held-out mean absolute error.

Run these in order. Item 1 decides whether the rest worked.

- [ ] 1. Measure the evaluator against played games.
      Labels come from `solve` at depth 2, and `solve` scores its own horizon
      with the committed weights, so the fit chases its own output.
      Add a bench that plays whole games from a fixed opening set.
      Report predicted win probability against realized outcome, in buckets.
      Held-out error moved 0.0957 to 0.0978 between two runs from a corpus
      change alone, so it cannot accept or reject a change.

- [ ] 2. Let the evaluator read the bench.
      `matchup_features` reads active against active only.
      A benched Pokemon contributes `alive_score` and `status_penalty` alone.
      Champions doubles brings four and leads two, so half of each team scores
      as a health bar.
      Add the best incoming matchup, the damage that the best switch-in takes,
      and the coverage of the remaining team against the opponent's remaining
      team.

- [ ] 3. Replace the bootstrap labels.
      Depth-2 labels suited a depth-3 search, where the evaluator only ranked
      leaves.
      A depth-1 search asks the evaluator to predict the rest of the game.
      Label from played-out results with the current bot on both sides.

- [ ] 4. Stop the replacement search from multiplying with damage rolls.
      A roll that faints a Pokemon opens a replacement node.
      `forced_descent` gives that node the remaining depth, not one less.
      At depth 1 the replacement runs a whole depth-1 search of its own.
      Doubles turns rise 93 times from one roll to sixteen, and nodes rise 140
      times, while the root matrix keeps its size.
      Merge faint branches that differ only in survivor health first.
      Score the replacement with a policy head when no depth remains.

- [ ] 5. Choose damage rolls by what they change.
      Sixteen rolls cost 93 times one roll and moved the value by 0.004.
      The support held five actions at every roll count.
      Read the rolls that cross a faint threshold.

- [ ] 6. Retire the dead features.
      `terrain_control` has zero variance, `terrain_edge` has 0.0004, and
      `guard_conditions` has 0.0003.
      The 758-team corpus holds no Grassy, Misty, or Psychic Surge team.
      Seed the corpus for these features, or remove them.
      Do this after item 1, so the removal can be shown to be safe.

Do not do these. The repo already measured each one.

- Do not add model capacity yet. `fitted_mlp` lost on the same features.
- Do not train on more positions. The learning curve is flat.
- Do not search deeper by default. One doubles depth-2 round is about eight
  minutes. `refine` reaches depth 2 on the cells that decide the answer.

# New features
