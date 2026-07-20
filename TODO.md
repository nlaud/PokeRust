# TODO: Always remove items from here when they are completed :)
### Fixes
- Archaludon blocked electro shot does not display +1 SpA Event but it does show up in the UI.: Turn 4
P1 sent out Incineroar (200 HP)

P1's Incineroar's Intimidate!

P2's Archaludon's Atk fell! (-1)

P2's Swampert's Atk fell! (-1)

P1's Gengar-Mega used Protect!

P1's Gengar-Mega protected itself!

P2's Archaludon used Electro Shot!

P1's Gengar-Mega blocked the attack!


- `random_doubles_battles_are_sound` / `random_doubles_beliefs_stay_sound_subset`
  (`poke_rust/src/tests/random_battle_tests.rs`): fog-of-war soundness fuzz tests.
  Full fix history (S1-S58) in memory `project_random_battle_fuzz_test_findings.md`
  and git log — not reproduced here.
  - **Contradiction oracle** (`random_doubles_battles_are_sound`, default test,
    runs on every `cargo test`): ~0.14% failure rate (10,000-battle sweep,
    2026-07-18). Two open families, neither root-caused: pass5 "every candidate
    nature is infeasible" panic (57% of failures — some derivation narrows a
    mon's stat window past what any nature/IV/EV combo can produce, no pattern
    found yet), `ItemLost` "clause has no explanation" panic (36%, FocusSash-heavy
    — reads like one recurring mechanism, next target if picked up).
  - **Subset oracle** (`random_doubles_beliefs_stay_sound_subset`, `#[ignore]`d —
    run via `cargo test --release -- --ignored ... --nocapture`): asserts the true
    state stays inside the belief's bounds, not just internally consistent.
    Baseline 36-37% (100-300 iter sweeps), down to ~28-35% after S56/S57, still
    ~32-33% after S58 below (S58 fixed a real bug but didn't move the aggregate
    rate — see why under S58). Kept `#[ignore]`d rather than folded into the
    default test since it still fails far more often than the contradiction
    oracle's historical 0.14%; fold it back in once the remaining families are
    closed. Open families:
    - **Nature/stat-window over-narrowing** (dominant family) — the Pass 3/5
      back-solve pipeline (`pass3_direction_a`/`pass3_direction_b`/`pass5_back_solve`)
      computes a bound that's wrong from the outset, not from a self-heal-catchable
      crossing. S56 (teamsheet EV double-scaling under `--stat-points` — a real
      core-engine bug, not just a fuzz artifact) was found and fixed here, but a
      post-fix sweep was statistically unchanged (37%), so at least one more
      unfound cause remains in this same pipeline. Repro + instrumentation
      methodology in memory.
    - **EV-lattice gated on `use_stat_points` (fixed, S58a)** — both
      `achievable_defender_hp_values` (Pass 3's HP-candidate enumeration) and
      `pass5_back_solve`'s own EV/IV back-solve used `EV_LATTICE` (the `8p-4`
      stat-points set) **unconditionally**, never checking
      `config.use_stat_points`. Under full-EV mode (`--stat-points false`) every
      EV 0-252 is legal, but the lattice only covers contribution 0 and every ODD
      contribution — every EVEN-contribution EV (8, 16, 24, ...) was silently
      untestable, able to skip a mon's true max HP/stat entirely (unsound
      exclusion) or, worse, wrongly narrow `min_evs`/`max_evs` to a coincidentally-
      matching but wrong EV (confirmed both failure modes with hand-built
      regression tests). Fixed by gating on `config.use_stat_points`, mirroring
      the pattern pass5's own EV-total-cap tightening already used. **Dormant
      under the fuzz config** (which always sets `use_stat_points: true`), so
      this is a real core-engine bug fixed proactively, not a fuzz-rate driver.
      Tests: `test_achievable_defender_hp_values_full_ev_mode_covers_off_lattice_hp`,
      `test_pass5_back_solve_full_ev_mode_covers_off_lattice_stat`
      (`inference_tests.rs`), both verified red/green.
    - **Def-specific 3-literal clause violations — partially fixed (S58b), one
      compounding issue remains.** `compute_defender_stat_bounds`'s E-B pruning
      dropped every type-mismatched berry from the defender's candidate-item set
      for **every** move — correct for a normal move (a berry that can't resist
      this move's type is provably inert), but wrong for **Knock Off**: its 1.5x
      power boost fires on ANY held (transferable) item regardless of that item's
      own effect, so a non-matching berry's mere PRESENCE still matters even
      though its resist effect doesn't fire. Confirmed via the standing
      Kingambit/Ariados-Knock-Off repro: Kingambit held a Chople Berry (Fighting-
      resist, irrelevant to a Dark-type move) until Knock Off's own removal effect
      took it — the neutral-gear search assumed no item (no boost), and could not
      reproduce the observed (boosted) damage at ANY BSV in range, producing the
      unsound `EVIVStatLE{Def,143}` bound (true Def was 155). Fixed by not pruning
      type-mismatched berries when the move is item-presence-dependent (currently
      just Knock Off). Test: `test_s58_knock_off_keeps_type_mismatched_berry_as_
      item_presence_escape` (`inference_tests.rs`), verified red/green — confirms
      the emitted clause now carries the `HasItem{ChopleBerry}` escape disjunct.
      **Not fully closing this family**: re-swept post-fix and the EXACT SAME
      Kingambit repro (`iter=5 turn=6`, `EVIVStatLE{Def,143}`) still panics.
      Traced further: the clause **is** emitted correctly with the full berry
      escape list at emission time (confirmed via direct instrumentation), but by
      the time the subset check runs, every `HasItem` literal has been stripped
      and only the bound literal survives — i.e. something downstream discards
      the escape disjuncts once the item later resolves to `Known(None)` (Knock
      Off's own removal, later in the same turn). Two untested candidate
      mechanisms: (a) `resolve_item_clauses_on_item_change`'s `outgoing: None`
      branch (item was still ambiguous, not `Known`, at the moment of change)
      wholesale-drops any clause mentioning the mon's item — plausible but a
      direct instrumentation check found zero calls into that function for this
      clause's own `mon_idx` before the panic, so probably not this; (b) more
      likely, ordinary BCP literal evaluation re-checking each `HasItem{X}`
      literal against the item's CURRENT (post-change, `Known(None)`) value once
      it resolves — sound in isolation (the item really is gone by then) but
      wrong here because the literal was about the state *at hit time*, not *now*
      — the same temporal-mismatch class S19/S57 fixed for other literal kinds,
      but the CNF store has no general notion of "this literal is scoped to a
      moment," so nothing currently protects a `HasItem` escape literal from
      being re-evaluated against a value the SAME event later changes. Next step:
      grep `bcp.rs` for wherever it drops literals that evaluate to `Some(false)`
      and confirm it isn't gated by an S19-style historical check for this path.
    - **Speed-order alternate-item/ability escape clauses** — dominant instance
      (a mon whose own move changes its own priority ability mid-resolution,
      e.g. Skill Swap racing `pass4_speed_from_order`) root-caused and fixed as
      S57 (three compounding timing bugs in `information/inference.rs`; re-swept
      37%→28%). Residual: the flinch-clause variant
      (`[HasItem{KingsRock}, HasItem{RazorFang}]`, shares item sets with the open
      `ItemLost` contradiction-oracle family above) confirmed NOT the same bug as
      S57 but not yet root-caused — apply the same live-ability/item-change
      timing lens first if picked up again.
    - **Illusion/Zoroark disguise tracking** — re-measured after S57: zero
      failures across 600 iterations (was consistently the top offender before).
      Likely mostly downstream of the speed-order bug; not proven zero, re-sweep
      before assuming a fresh cause if it resurfaces. The underlying structural
      hazard (whichever mon's primary infeasibility fires first gets promoted,
      even when caused by an unrelated bug) is still real and undocumented in
      `information/README.md`.
  - Config: fuzz test uses `champions_legal_items()` (curated catalog, not the
    full ~1,000-item pool) — confirmed this wasn't the primary driver of the
    subset-oracle rate, kept anyway as the more realistic default.
### New features
- Link all the READMEs to the main project README, put links to all the READMEs in AGENTS/CLAUDE.md if it isn't there already.
- Frontend features
  - Add benchmarking to the FE somewhere (right aligned of navbar?)
- Test the meta sampler & make sure a chunk of the data looks good...
  - Now on the Rust end, create a determinizer that takes in the meta state (we will need a parser for this) + an unknownbattlestate, then, like the simulator, has modes for giving a single random sample state or giving every possible state with probability (This should not consider "other" options, just the normal ones, it should force based on the meta percent and known information items, moves, etc.). It should output complete full states though that should just be able to be put in the simulator and work!
- Eventually create nash solver and recursive evaluation (When both players have perfect information)
  - [ ] Tracker page: needs a parser for lines of input -> action / reaction tree
        (figuring out what is a reaction to what and what causes what from just the
        lines, also must add guaranteed effects so the user doesn't need to put those
        in manually). This will likely include some RegEx stuff as well. Need a
        detailed spec.
  - [ ] Then move on to actual bot creation, battle and mentor pages.
