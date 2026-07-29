# TODO: Always remove items from here when they are completed :)

### Fixes
- Rewrite documentation + comments in STE

- Remove "The three sweeps run one after another so their timings stay directly comparable to the recorded numbers in poke_rust/benches/RESULTS.md; each chart fills in the moment its own sweep finishes rather than waiting for the whole run. Hover a dotted underline for what a setting or column means" from benchmarking
Tracker improvements:

- Fox hovering tooltips, they jsut show a ?mark 

- [ ] Model every temporally valid silent-entry path for weather-setting
  abilities, then re-enable weather-setter absence narrowing. It is currently
  disabled conservatively: real-team tracker fuzz found `SnowWarning` could be
  silent on one entry and directly revealed on a later entry even after the
  obvious matching-weather and primordial-weather no-op cases were handled.
### New features
Determinizer follow-ups — **done**. Nature/spread coherence ships on at `0.15`,
applied per-nature-row so `P(nature)` stays pinned to the usage rate;
`pre_transform` / `illusion_disguise` / `rest_sleep` all round-trip; the bench
prior is bidirectional and multiplicative. Three bugs surfaced along the way and
are fixed: the 510 EV cap (S68), the invented-bench item-clause leak, and the
subset oracle deriving pre-nature stats for Transformed mons.

Left over from that work, none of it blocking:

- `nature_spread_coherence` is one knob damping two rules of very different
  evidential strength. Measured across 235 species, "nature raises a stat with 0
  points" is 12.9pp of the 19.4% incoherent baseline while "nature lowers an
  invested stat" is only 3.9pp — but the first is much weaker evidence, since a
  nature boost scales the whole base stat (Careful with 0 SpD EVs still gains
  ~15 SpD off base 90, a build people run). Consider splitting into two
  multipliers, ~0.10 lowered / ~0.35 raised.
- `sample_uniform_spread` still bypasses coherence entirely. It only runs once
  the belief has excluded every authored spread, and its doc calls that the
  honest maximum-entropy answer — but it is also exactly where incoherent builds
  are most likely.
- `meta/team_gen.rs`'s `sample_nature`/`sample_spread` still draw independently,
  so generated teams carry the ~19.4% incoherent rate the determinizer no longer
  does. Fixing it is conditional renormalization for free (the nature is already
  fixed, so there is no marginal to preserve).
- `collect_natures` (`meta/dex.rs`) pushes duplicate `stat_alignment` rows that
  resolve to the same `Nature` — avalugg lists Adamant at both rank 1 (36.2%)
  and rank 4 (12.2%). The true marginal is the sum, so any nature-marginal
  assertion is wrong by ~12pp for those species. Merge, or warn.
- `check_determinization` has no duplicate-item check. Adding one would make the
  item clause a permanent invariant rather than something a single test watches,
  but it needs `&DeterminizeConfig` in a public signature.

VGC solver target definition (rechecked against the July 2026 repository and
current official tournament handbook):

- The project actually has three different games to solve and should name them
  separately in configs, benchmarks and UI: perfect information (the existing
  correctness oracle), current tournament open-list play, and closed-sheet
  ladder/tracker play. The 2026 Championship Series shares every team-list field
  except numeric stats; because stat alignment/nature is also on that list,
  `OpenTeamSheetNatures` is the closest existing information mode—not the current
  `closedSheet` default. Keep closed sheet supported, but do not tune the
  tournament solver primarily against a harder and different information game.
- Regulation now governs Tera and Mega availability from saved format through
  preview, concrete battle, fog state, tracker and command enumeration. Extend
  that same regulation model to legal rosters as Champions rules rotate. Do not
  spend search budget enumerating a species or mechanic merely because the engine
  can simulate it; benchmark the action space players actually have.
- Official doubles selects four Pokémon and sends the first two in party order.
  From six Pokémon that is `P(6,2) × C(4,2) = 180` bring/lead actions per player,
  or 32,400 preview matrix cells before evaluating a single battle. Team preview
  is therefore a first-class simultaneous imperfect-information decision, not a
  setup detail; the current solver explicitly rejects it.
- Current limits are 90 seconds for Team Preview, 45 seconds per move, seven
  minutes of player time and 20 minutes per game. The bot/interactive profile
  should produce a useful checkpoint far below 45 seconds and accept an explicit
  deadline; offline mentor analysis can spend longer. The simulator currently
  has no wall-clock/game-time state, so its “win probability” does not value
  timeout/tiebreak play. Either model that state eventually or label the solver's
  objective as no-timeout battle win probability.
- Championship matches are commonly best-of-three and the same hidden numeric
  build persists across games. A match-analysis mode should carry posterior
  knowledge and opponent tendencies between games, reuse team-preview/battle
  work, and optimize match win probability. A per-game equilibrium remains the
  simpler first target.
- Ordinary turns are simultaneous joint commands over two coupled active slots.
  Faints and pivot moves introduce replacement/self-switch decisions that do not
  consume a turn-depth ply. Random damage, accuracy, critical hits, secondary
  effects, speed ties and order-dependent interactions make this both a
  simultaneous stochastic game and, under fog, a two-sided private-information
  game. Any proposed method must cover all three properties rather than solving
  only “large minimax.”

- [x] Nash solver and recursive evaluation for perfect-information positions —
  `poke_rust/src/solver/`. Simultaneous-move backward induction with double-oracle
  pruning and serialized alpha-beta bounds (Bošanský et al., AIJ 2016), an exact
  matrix-game equilibrium solver, and `cargo bench --bench solver_speed`.
  - [ ] Build an anytime, iterative-deepening wrapper. Complete depths 1, 2, …
    inside a wall-clock/node budget, retain the last completed result when a
    deeper iteration is cancelled, and reuse the previous depth's root support,
    action ordering and resolved transitions. A payoff/value from depth `d` is
    not an exact payoff at depth `d+1`; use it only as a prior/order hint and keep
    transposition values depth-keyed. The exact double-oracle search remains the
    shallow-search oracle and regression baseline; deeper approximate methods
    must continue to agree with it on the small positions they can both finish.
  - [ ] Add team-preview solving. Start with perfect/open-list preview, lazy
    double oracle over the 180 choices, a fast preview policy/value model, and
    cached battle values for requested preview cells. The current meta cache has
    teammate/build marginals but no bring, lead, matchup or win-rate labels, so it
    cannot honestly supply a preview policy; acquire replay/battle-log data or
    generate self-play labels instead of treating teammate rank as lead rate.
  - [ ] Make VGC doubles action selection progressive instead of using the current
    stride-based hard cap. The present cap first removes every Tera/Mega action
    whenever any plain action survives, so a capped search is unsuitable for
    actual play even though it is useful for benchmarks. Seed the restricted game
    with the preceding depth's support, shallow-search scores and eventually a
    learned policy; add actions through best-response checks over the entire
    uncapped legal set. Generate candidates lazily by resource allocation
    (neither/which slot Teras or Megas), per-slot command and target rather than
    materializing then discarding the Cartesian product. Remove provably duplicate
    or dominated joint commands where possible. Per-slot factorization is only an
    experimental proposal: targets, spread moves, redirection, Protect, switching
    and the shared Tera/Mega resource make the two slots strongly coupled.
  - [ ] Add an approximate deep-search lane and benchmark it against exact shallow
    double oracle:
    - Simultaneous-move MCTS with regret matching and/or Exp3 is the most direct
      anytime baseline. Keep explicit exploration and average strategies; the
      convergence assumptions are stricter than merely plugging an arbitrary
      bandit into every node.
    - Monte Carlo *-minimax / sparse chance sampling is a strong candidate for
      this engine because turn simulation and chance branching, not matrix LPs,
      dominate cost. Preserve exact enumeration as the test oracle. Report
      sampling error and do not present renormalized Top-K outcomes as exact:
      rare damage/critical/secondary-effect tails can cross a VGC KO threshold.
    - Try progressive widening or sampled action subsets only after the full legal
      action oracle is retained for periodic exploitability checks.
  - [ ] Add a true generative transition interface for sampled search. The current
    `ChanceMode::Sample` first constructs the complete `simulate_turn`
    distribution and only then samples successors, so it does not avoid the
    expensive combinatorial doubles resolution that reached 78,630 branches at
    four rolls. Outcome-sampling MCTS/MCCFR needs to sample inside turn resolution
    (using the existing sample-mode chokepoints) and return next state, public and
    per-player observations, true trajectory probability and sampling probability.
    Keep exact enumeration beside it as the oracle. Investigate stratifying the
    high-impact hit/crit/secondary/speed-tie events while sampling damage rolls,
    plus common random numbers/control variates for comparing root actions.
  - [ ] Calibrate or replace `solver::eval::heuristic`. Fit a value model from
    deeper-search/self-play labels, and a policy prior for action ordering and
    progressive widening. VGC features should cover speed order and control
    (Tailwind/Trick Room), damage and KO ranges, Protect pressure, targeting and
    redirection, weather/terrain/screens, board position, bench/switch resources,
    status/boosts, and remaining Tera/Mega resources. Validate on held-out
    positions with calibration/Brier or log loss, side-swap symmetry, move-policy
    agreement and self-play—not just prediction loss. Also test active-slot
    permutation invariance and use roster/field encodings rather than tying the
    network to species IDs or one regulation. The current `fn(&BattleState) ->
    f64` evaluator interface cannot batch or receive dex/belief context; plan an
    evaluator service/API before choosing a model runtime. Batch leaf inference
    so a model does not turn one cheap heuristic call into the new bottleneck.
  - [ ] Treat “solve every determinized world independently, then average its
    strategies” as a labelled perfect-information Monte Carlo baseline only, not
    the fog-of-war architecture. It has strategy fusion: the solver effectively
    chooses a different action for hidden worlds in the same information set.
    ISMCTS is a useful fast heuristic baseline and POMCP is useful only after an
    opponent policy turns the problem into a POMDP; neither by itself gives the
    desired adversarial two-player equilibrium. Make Online Outcome Sampling /
    outcome-sampling MCCFR the first principled fog baseline, then investigate
    public-belief continual re-solving (ReBeL/DeepStack/Libratus family). Keep the
    independent-determinization result to quantify how optimistic strategy fusion
    is.
  - [ ] Once an extensive-form information-set interface exists, compare MCCFR
    with sequence-form/extensive-form double oracle (XDO/PDO). The existing
    perfect-information algorithm solves a separate matrix at each concrete
    state; it cannot simply be called inside fog because strategies and reach
    probabilities are coupled across indistinguishable histories. An
    extensive-form DO can exploit small supports at every information set, which
    fits VGC's many legal but rarely useful joint actions, but known exponential
    lower bounds and expansion-frequency sensitivity make it a benchmarked
    candidate rather than the assumed winner.
  - [ ] Build the game representation fog solving actually requires before
    selecting an algorithm:
    - The current beliefs are observer-centric sets/bounds, while equilibrium
      search needs weighted reach beliefs over both players' private information.
      In open-list play each player knows its own exact numeric build and not the
      opponent's, so treating the opponent as fully informed about P1 is a
      conservative but different game.
    - The determinizer's returned draw probability is not a normalized posterior,
      and its build marginals are intentionally approximate. Add weighted
      particles/posterior updates, effective-sample-size diagnostics and
      resampling/rejuvenation; distinguish prior-model error from search error.
    - Group concrete successor worlds by what each player observes, not by
      `MatchState` hash. Damage is exact for one's own side but percentage-masked
      for the opponent, and move/order/reveal events update beliefs. The raw event
      stream plus `mask_events_for` is the natural observation kernel.
    - At an imperfect-information depth limit, one concrete state's scalar value
      is generally not well-defined independently of ranges/reach strategies.
      Train/evaluate public-belief counterfactual value vectors (or multivalued
      opponent continuations), not merely the perfect-information scalar
      heuristic averaged across particles.
  - [ ] Offer opponent exploitation as a separate, explicit mode. Nash is the
    robust default, but human VGC opponents are not equilibrium policies and a
    best-of-three provides observations. Restricted Nash Response / safe
    opponent-exploitation can trade a displayed exploitability budget for value
    against a learned opponent model. Never silently mix this objective into the
    equilibrium result.
  - [ ] Integrate the solver into the simulator as an optional P2 bot, with a
    configurable bot profile attached to battle creation. Expose at least
    perfect-information vs. P2-belief play, robust Nash vs. explicitly-labelled
    exploitative play, exact/approximate algorithm, time/node budget, maximum
    depth, worker count, chance sampling, seed/reproducibility and action-cap
    policy; surface every approximation and fallback in the UI rather than
    silently weakening the game.
    - Start P2's cancellable analysis job immediately after a turn resolves and
      legal commands for the new state are known, while P1 is reading the board
      and considering moves. Search from an immutable state/belief snapshot and
      reuse all completed depths/checkpoints until P1 submits or the state
      generation changes. This turns human think time into bot compute time
      without extending the turn wait.
    - Preserve simultaneous-move semantics: P2's strategy may depend on P2's
      regulation-appropriate information state, but never on P1's hovered,
      selected or submitted command. The server must not send P2's live strategy
      to the P1 client before resolution. When P1 commits, sample P2's joint
      command once from the latest complete mixed strategy using the configured
      stable seed, then resolve both commands together.
    - Define early-submit and failure behavior. If P1 acts before any checkpoint,
      either wait up to a configured bot response deadline or use a clearly
      labelled shallow/policy fallback; never use a half-solved payoff matrix as
      an equilibrium. If the solver fails, exhausts its budget or is cancelled,
      retain the last valid completed checkpoint. Perfect-information bot mode
      may use ground truth only when explicitly selected; ordinary simulator bot
      mode must use P2's own belief and observation history.
    - Give every battle turn a solver generation ID. Cancel superseded jobs on
      turn resolution, battle deletion, disconnect or configuration change, and
      ignore stale completions. Share the bounded solver CPU pool with the live
      analysis API under explicit per-session quotas so an idle-thinking bot
      cannot starve turn resolution or benchmarks.
    - Add simulator controls for Human vs. Bot P2 and named bot presets
      (fast/balanced/strong/custom), a private progress indicator showing that P2
      is thinking without leaking its policy, deterministic replay metadata, and
      post-turn diagnostics that reveal the sampled action and the strategy it
      came from only after both moves are locked. Test that P1 selection changes
      do not restart or influence P2 search, the same seed reproduces the sampled
      bot command, fog mode never reads hidden P1 state, stale generations cannot
      submit, and bot turns remain legal through normal, replacement and pivot
      phases.
  - [ ] Then add mentor-page integration, possibly as an optional simulator or
    tracker panel, using the same streamed checkpoints but showing the requesting
    player's recommendation rather than autonomously submitting it.

VGC solver parallelization plan:

- [ ] Use a dedicated bounded work-stealing CPU pool, separate from Tokio and
  from benchmark workers. Benchmark physical-core-sized pools rather than
  assuming logical-core count is best. Keep the matrix solve and double-oracle
  control loop serial—they are cheap and sequential—and parallelize the expensive
  simulations requested by that loop:
  1. missing cells in the current restricted matrix;
  2. candidate rows/columns during each full best-response sweep;
  3. independent public-belief particles or sampled chance successors when the
     first two sources do not expose enough work.
- [ ] Use a different parallel shape for the eventual fog solver: generate
  batches of independent MCCFR/OOS trajectories from a frozen strategy/regret
  snapshot, accumulate thread-local regret/strategy deltas, then merge at a
  deterministic barrier. Compare batch staleness against lock contention rather
  than doing unsynchronized atomic updates at every information set. A learned
  leaf evaluator can add batched GPU inference later; the current bottleneck is
  CPU turn simulation, so moving LPs or small regret tables to a GPU first is
  unlikely to matter. Recent parallel-CFR work suggests information-set and
  tree-node parallelism, but it must be revalidated on this dynamic simulator
  rather than assumed from materialized poker trees.
- [ ] Do not eagerly compute the full root matrix merely to fill all cores; that
  destroys the cell savings double oracle is providing. Do not prioritize
  parallel serialized alpha-beta either: the existing benchmark already shows
  that its extra turn simulations outweigh its matrix-cell savings.
- [ ] Give each worker a local search context, RNG stream, statistics and
  transposition/turn cache first. Derive seeds from stable job IDs and merge
  results in a stable order so scheduling cannot change floating-point sums.
  Later benchmark a sharded shared transposition table against duplicated work;
  do not add a mutex to every node without evidence that it wins.
- [ ] Bound the number of in-flight jobs so depth-first memory remains roughly
  `O(workers × depth × branching)`. Add cooperative cancellation/deadline and
  generation checks between cell, successor and node expansions; unlike the
  benchmark's `spawn_blocking` sweeps, an interactive analysis must actually stop
  when its client disconnects or supersedes it.
- [ ] Measure 1/2/4/… worker scaling on a fixed corpus of representative,
  preferably uncapped VGC doubles positions. The current Rock Slide/Heat Wave
  stress case is valuable but not a playing-strength corpus. Stratify early/mid/
  late game, normal/replacement/pivot phases, high/low action counts, Tailwind,
  Trick Room, weather, redirection, priority, setup, spread attacks and Protect/
  switch-heavy boards. Record wall time at 50 ms/200 ms/1 s/5 s/15 s/40 s, time
  to first useful strategy, turns simulated, duplicate work/cache hits, peak RSS,
  and reproducibility. On solvable slices measure exploitability; elsewhere use
  paired head-to-head/self-play, policy stability and deeper-search agreement.
  Reject speedups that mostly come from changing the searched game or inflate
  simulation count enough to hurt deeper searches.

Live solver results in the frontend:

- [ ] Add a cancellable analysis job API rather than copying the uncancellable
  benchmark endpoint. A practical shape is `POST /api/solve` followed by an SSE
  stream at `/api/solve/{id}/events`; starting a new generation cancels the old
  one, and stale generations are ignored by the client.
- [ ] Stream `started`, `update`, `done`, `failed` and `cancelled`. Every update
  needs a stable generation/revision, completed and target depth, elapsed time,
  exact/approximate/provisional status, P1 value, the complete mixed strategy over
  stable action IDs, and search statistics. Approximate results also expose
  sample/particle count, effective sample size, confidence/error information,
  omitted chance mass, action-cap warnings, information mode, prior/model version
  and whether the objective includes game/match timeout.
- [ ] Publish after every fully completed depth. Within a double-oracle depth,
  publish only after solving the restricted game and completing both full
  best-response sweeps: for P1 utility, those certify
  `[min_column u(root_strategy, column),
  max_row u(row, opponent_strategy)]`. Show that interval/exploitability gap.
  This is a certificate only when cells are exact and the sweep covers the full
  uncapped legal action set; with sampled cells call it an empirical search gap
  and attach confidence, and with a depth/value horizon say “depth-limited gap,”
  not full-battle exploitability. An Anytime Double Oracle variant is worth
  testing if monotonically improving exploitability between all UI updates
  matters. Never display an equilibrium of an arbitrarily half-filled payoff
  matrix as though it were valid.
- [ ] Present a mixed recommendation, not just “best move.” The current argmax
  can be highlighted, but show each supported joint action's probability, depth,
  exact/approximate badge, value interval or confidence, and whether the
  recommendation changed since the last completed checkpoint. Keep the last
  completed-depth card visible while the next depth is running. Equilibria need
  not be unique, so an argmax/probability can jump while value and exploitability
  remain stable; show support overlap/strategy distance and value stability
  rather than treating every tie-driven policy change as new tactical evidence.
- [ ] Coalesce/rate-limit progress events (roughly animation-frame/user-readable
  frequency, not one event per node), and make action IDs independent of display
  labels. Search completion may suggest an action but must not submit it
  automatically.

Research basis for the solver roadmap:

- The Pokémon Company, [Play! Pokémon Video Game Championships Tournament
  Handbook](https://mcdn.pokemon.com/pokemon-prod/raw/upload/v1/live/static-assets/content-assets/cms2/pdf/play-pokemon/rules/play-pokemon-vgc-tournament-handbook-en.pdf)
  — current open-list contents, choose-four doubles, best-of-three guidance and
  the 90-second preview / 45-second move / seven-minute player / 20-minute game
  limits; the official [Champions gameplay
  page](https://champions.pokemon.com/en-us/gameplay/) confirms rotating
  regulations and Mega Evolution in the initial rules. These rules change, so
  recheck them when the target regulation changes.
- Bošanský et al., [Algorithms for computing strategies in two-player
  simultaneous move games](https://www.sciencedirect.com/science/article/pii/S0004370216300285)
  — the exact/Monte Carlo simultaneous-game foundation already used here.
- Lisý et al., [Convergence of Monte Carlo Tree Search in Simultaneous Move
  Games](https://papers.nips.cc/paper_files/paper/2013/hash/1579779b98ce9edb98dd85606f2c119d-Abstract.html)
  and the [follow-up convergence
  analysis](https://arxiv.org/abs/1804.09045) — SM-MCTS, regret matching/Exp3,
  exploration and the caveats around local no-regret guarantees.
- Lanctot et al., [Monte Carlo *-Minimax
  Search](https://www.ijcai.org/Proceedings/13/Papers/093.pdf), and Kearns et al.,
  [Sparse Sampling](https://www.ijcai.org/Proceedings/99-2/Papers/093.pdf) —
  sampled planning for densely stochastic trees.
- Dinh et al., [Online Double
  Oracle](https://eprints.soton.ac.uk/471822/2/online_double_oracle.pdf), McAleer
  et al., [Anytime Double
  Oracle](https://openreview.net/pdf?id=J2TZgj3Tac), and Tang et al.,
  [Regret-Minimizing Double
  Oracle](https://proceedings.mlr.press/v202/tang23b.html) — anytime restricted
  games, effective supports and exploitability-aware iteration.
- Bošanský et al., [Double Oracle for Zero-Sum Extensive-Form
  Games](https://www.cs.utep.edu/kiekintveld/papers/2013/bklcp-DO.htm), McAleer
  et al., [XDO](https://arxiv.org/abs/2103.06426), and Zhang and Sandholm,
  [Exponential Lower Bounds on Double
  Oracle](https://www.ijcai.org/proceedings/2024/0336.pdf) — how to extend
  restricted games across information sets and why small empirical supports do
  not remove the need for worst-case/expansion benchmarks.
- Cowling et al., [Information Set Monte Carlo Tree
  Search](https://orangehelicopter.com/academic/papers/tciaig_ismcts.pdf), Silver
  and Veness, [POMCP](https://proceedings.neurips.cc/paper/2010/hash/edfbe1afcf9246bb0d40eb4d8027d90f-Abstract.html),
  and Long et al., [Understanding the Success of Perfect Information Monte Carlo
  Sampling](https://ojs.aaai.org/index.php/AAAI/article/view/7562) — belief
  particles, heuristic shared information-set search and determinization failure
  modes; POMCP's guarantees are for a POMDP, not an adversarial two-player game.
- Zinkevich et al., [Regret Minimization in Games with Incomplete
  Information](https://papers.nips.cc/paper_files/paper/2007/hash/08d98638c6fcd194a4b1e6992063e944-Abstract.html),
  Lanctot et al., [Online Outcome
  Sampling](https://www.mlanctot.info/files/papers/aamas15-iioos.pdf), and Schmid
  et al., [Variance Reduction in
  MCCFR](https://ojs.aaai.org/index.php/AAAI/article/view/4048) — the principled
  fog-of-war baseline and how sampled counterfactual estimates can be made useful
  under a simulator budget.
- Brown et al., [ReBeL](https://arxiv.org/abs/2007.13544),
  Moravčík et al., [DeepStack](https://dmorrill10.github.io/assets/publications/17science.pdf),
  and Brown and Sandholm, [Libratus](https://www.ijcai.org/Proceedings/2017/772)
  — public-belief search, depth-limited values and continual/safe re-solving.
- Kovařík et al., [Value Functions for Depth-Limited Solving in Zero-Sum
  Imperfect-Information
  Games](https://www.sciencedirect.com/science/article/pii/S000437022200145X),
  Brown et al., [Depth-Limited Solving for Imperfect-Information
  Games](https://proceedings.neurips.cc/paper/2018/hash/34306d99c63613fad5b2a140398c0420-Abstract.html),
  and Solinas et al., [History
  Filtering](https://proceedings.neurips.cc/paper_files/paper/2023/hash/87ee1bbac4635e7c948f3eea83c1f262-Abstract-Conference.html)
  — why fog-of-war leaves need public-belief/counterfactual values and why
  constructing the corresponding root histories can itself be expensive.
- Veness et al., [Variance Reduction in Monte-Carlo Tree
  Search](https://papers.neurips.cc/paper_files/paper/2011/hash/d736bb10d83a904aefc1d6ce93dc54b8-Abstract.html)
  and Davis et al., [Low-Variance Baselines for Extensive-Form
  Games](https://proceedings.mlr.press/v119/davis20a.html) — common random
  numbers, control variates and baseline-corrected trajectory estimates.
- Ponsen et al., [Computing Approximate Nash Equilibria and Robust Best
  Responses Using Sampling](https://arxiv.org/abs/1401.4591) — Restricted Nash
  Response as an explicit safety/exploitation trade rather than silently
  assuming a human opponent is optimal.
- Chaslot et al., [Parallel Monte-Carlo Tree
  Search](https://cris.maastrichtuniversity.nl/en/publications/parallel-monte-carlo-tree-search/)
  and Liu et al., [On Effective Parallelization of Monte Carlo Tree
  Search](https://starai.cs.ucla.edu/papers/LiuDRLW20.pdf) — work stealing,
  virtual loss and the duplicate-work/search-quality costs of parallel search.
- Kim and Sandholm, [Parallelizing Counterfactual Regret
  Minimization](https://arxiv.org/abs/2605.14277) — a recent preprint suggesting
  information-set/node linear-algebra parallelism; useful direction, not yet
  evidence that GPU CFR beats CPU simulation for this engine.
- Hubert et al., [Learning and Planning in Complex Action Spaces
  (Sampled MuZero)](https://proceedings.mlr.press/v139/hubert21a.html) — policy-led
  action sampling as a later approximate-search option, not an exact substitute.
