# TODO: Always remove items from here when they are completed :)

### Fixes

- `random_doubles_battles_are_sound` / `random_doubles_beliefs_stay_sound_subset`
  (`poke_rust/src/tests/random_battle_tests.rs`): fog-of-war soundness fuzz tests.
  Full fix history is in the regression tests and git log.
  - **Contradiction oracle** (`random_doubles_battles_are_sound`, default test):
    historical 10,000-battle sweep was ~0.14% failures (2026-07-18). The known
    families were Pass 5 “every candidate nature is infeasible” and an
    `ItemLost` “clause has no explanation” path. Re-measure after the subset
    fixes before treating that old rate as current.
  - **Subset oracle** (`survey_subset_violations`, ignored diagnostic): the
    deterministic 1,000-match acceptance sweep is now **3/1000 failures (0.3%)**
    (**0 field violations, 3 clause reports, 1 caught contradiction panic**;
    2026-07-20), down from the original 36–37% baseline. A separate 300-match
    sweep completed at **0/300**. This clears the <1% target. Keep the exhaustive survey
    ignored until the residual cases are closed; run it with:

    ```powershell
    $env:POKERUST_FUZZ_SEED_START='0'
    $env:POKERUST_FUZZ_ITERS='1000'
    cargo test --release survey_subset_violations -- --ignored --nocapture
    ```

    Remaining deterministic failures are item/order clauses: seed 350 turn 24
    (`Quick Claw ∨ Iron Ball ∨ Custap Berry`) and seed 454 turn 2
    (`Quick Claw ∨ Choice Scarf`), plus one caught contradiction not yet
    isolated. The two seed-350 reports occur in one match. Doubles build inference
    now deliberately restores a conservative species-level nature/EV/IV/stat
    envelope after inference; this eliminated the remaining false damage/stat
    exclusions while preserving non-build inference.

  - The fuzz configuration uses `champions_legal_items()` and stat-points mode.
    Deterministic seeds and replay controls are available through the
    `POKERUST_FUZZ_*` environment variables documented in the test module.

### New features
- Cache the benchmarking so that it stays even when you switch tabs until you re-run them.
- Test the meta sampler & make sure a chunk of the data looks good...
  - Now on the Rust end, create a determinizer that takes in the meta state (we will need a parser for this) + an unknownbattlestate, then, like the simulator, has modes for giving a single random sample state or giving every possible state with probability (This should not consider "other" options, just the normal ones, it should force based on the meta percent and known information items, moves, etc.). It should output complete full states though that should just be able to be put in the simulator and work!
- Eventually create nash solver and recursive evaluation (When both players have perfect information)
  - [ ] Tracker page: needs a parser for lines of input -> action / reaction tree
        (figuring out what is a reaction to what and what causes what from just the
        lines, also must add guaranteed effects so the user doesn't need to put those
        in manually). This will likely include some RegEx stuff as well. Need a
        detailed spec.
  - [ ] Then move on to actual bot creation, battle and mentor pages.
