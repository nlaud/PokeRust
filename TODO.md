# TODO
### Fixes
- [x] Add inference to the benchmarking, sampling from all the teamsheets in the
  folder: added `poke_rust/benches/battle_sweep.rs` (full doubles battles across
  all 25 ordered teamsheet pairings, each resolved once and replayed against all
  4 information modes so `apply_information` cost is timed fairly on an
  identical event stream) and reworked `benches/turn_speed.rs` to sweep the same
  pairings with seeded random leads/moves instead of two fixed teams (enumerate
  mode capped at ≤4 damage rolls — full enumeration is the >15 GB doubles risk
  CLAUDE.md flags, and randomized moves make branch counts unpredictable ahead
  of time; sample mode keeps the full 1-16 roll grid). Command selection and
  belief seeding are ported from the proven `random_battle_tests.rs` fuzzer,
  shared between both benches via the new `benches/bench_common.rs`. Seeding
  only reproduces the harness's first draw (team-preview leads) exactly — the
  engine's own entropy-based RNG (damage rolls/crits/misses) feeds back into
  which commands are legal on later turns, so `battle_sweep`'s full trajectory
  varies run to run; `turn_speed`'s single post-preview turn is unaffected and
  reproduces exactly (verified by diffing branch counts across two runs).
  `battle_sweep` also surfaces, at low frequency (~1-2% of calls), the same two
  known inference contradictions tracked below — expected, not a bench defect.
- `random_doubles_battles_are_sound` (poke_rust/src/tests/random_battle_tests.rs) currently fails — found two real inference-engine soundness bugs, both unfixed:
  - Pass 5 nature back-solve hits a crossed min/max stat bound (`information/inference.rs:8732`) — some earlier pass (likely Pass 3's damage-based stat inversion) narrows a bound past an existing one without detecting the crossing. Seen on Def, SpA, and SpD, so systemic not stat-specific.
  - Learnset-based Illusion narrowing (`information/inference.rs:3449`, `check_move_legal_for_species`) misfires on ordinary moves (Aqua Jet, Rock Slide, Scald, etc.) almost any time a Zoroark-line Pokémon is on the field. Every existing test disables this feature (`learnset_dex: HashMap::new()`), so it likely has never run against real data before — suspect `parse_learnset_dex` (`state/dex_data.rs:2016`) is mis-parsing the learnset file (e.g. missing TM/egg moves). See memory `project_random_battle_fuzz_test_findings.md` for full repro details.
### New features
- Link all the READMEs to the main project README.
- Frontend features
  - We need a favicon lul
- Eventually create nash solver and recursive evaluation (When both players have perfect information)
- Create a meta sampler from pikalytics, and then get the algorithm to understand that
  - [ ] Tracker page: needs a parser for lines of input -> action / reaction tree
        (figuring out what is a reaction to what and what causes what from just the
        lines, also must add guaranteed effects so the user doesn't need to put those
        in manually). This will likely include some RegEx stuff as well. Need a
        detailed spec.
    - https://stitch.withgoogle.com/projects/6512361286860616575 for designs
    - https://github.com/PokeAPI/sprites for FE sprites (fetched at runtime, never committed)
  - [ ] Then move on to actual bot creation, battle and mentor pages.
