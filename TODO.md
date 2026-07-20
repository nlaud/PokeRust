# TODO
### Fixes
- ~~Unseen Fist not working~~ **Fixed.** Researched on Bulbapedia: Unseen Fist lets
  contact moves bypass Protect/Detect/King's Shield/Spiky Shield/Baneful
  Bunker/Quick Guard/Wide Guard (every `ProtectKind` this engine implements — the
  sole documented exception, Max Guard, has no implementation here), but in
  Pokémon Champions specifically the bypassed hit deals only 1/4 damage, not full
  damage like older generations. Implemented as `unseen_fist_mult` folded into the
  core damage formula (`simulator/helpers.rs`), with the per-target protect-block
  loop in `simulator/mod.rs` letting the hit fall through (still applying the
  blocking move's own contact punishment — Spiky Shield chip / Baneful Bunker
  poison / King's Shield −1 Atk — since that's independent of the block itself)
  and emitting `AbilityRevealed`. Also added `pass2_unseen_fist_absence`
  (`information/inference.rs`) so a genuine, unrevealed `Blocked` on a contact
  move excludes Unseen Fist from the attacker's belief (gated on ability
  suppression, mirroring `pass2_contact_absence`). Tests: `protect_moves` module
  in `simulator_tests.rs` (4 tests) + `inference_tests.rs` (2 tests).
- ~~"P2's Incineroar was cured of put to sleep!" should say woke up~~ **Fixed** —
  `frontend/src/lib/eventText.ts`'s `statusCured` case now special-cases
  SLP→"woke up" / FRZ→"thawed out"; other statuses keep "was cured of X".
- ~~Choice Lock volatile visible on opponents without enough information~~
  **Fixed.** Unlike every other volatile in this engine, Choice Lock is a silent
  consequence of a still-hidden held item (no in-game message announces it).
  `mask_pokemon_view` (`bin/server/mapping.rs`) now re-derives an opponent's
  rendered volatiles from ground truth and drops `ChoiceLock` unless the belief's
  item is `Known` to a Choice item (or its live Illusion hypothesis is). Tests:
  `choice_lock_masking_hidden_when_item_unconfirmed` /
  `..._shown_when_item_confirmed_choice` in `mapping.rs`.
- ~~Renders "not &lt;item&gt;" for items not in the current format~~ **Fixed** —
  two parts: (1) `describe_unknown_item`'s `Some(pool)` branch
  (`information/describe.rs`) now intersects the excluded list with the format's
  pool before rendering "not X", falling back to "Unknown" if nothing in-format
  was actually excluded. (2) The format's legal-items whitelist wasn't threaded to
  the server at all (`legal_items: None` hardcoded) — `StoredFormat.bannedItems`
  is now resolved against the catalog (`lib/items.ts`) into
  `CreateBattleRequest.legalItems` (`SetupPanel.tsx`), parsed server-side into
  `InferenceConfig.legal_items` (`routes.rs`, validated up front — an
  out-of-catalog item on either team now 422s at battle creation instead of
  panicking mid-battle), and threaded through `mapping.rs`'s whole render chain
  (`battle_view`→`side_view`/`preview_view`→`mask_pokemon_view`). Tests: 2 new
  `describe_unknown_item` unit tests + `side_view_item_text_hides_out_of_format_exclusions`
  in `mapping.rs`.
- `random_doubles_battles_are_sound` (poke_rust/src/tests/random_battle_tests.rs): fog-of-war soundness fuzz test. Long fix history (S1–S54) in memory `project_random_battle_fuzz_test_findings.md` — not reproduced here. A 10,000-battle sweep (2026-07-18) measured the current failure rate at **0.14% (14/10,000)**, split across three open families:
  - **pass5 "every candidate nature is infeasible" panic** (57% of failures, 8/14). Some derivation narrowed a mon's stat window past what any nature/IV/EV combo can produce. Confirmed NOT caused by the already-fixed S54 mechanism (HP-dependent base power) — stats/moves vary each instance (Spe, SpA, SpD; LastRespects, IcyWind, ExtremeSpeed, FlipTurn, MegaEvolution turns), no single pattern yet. Not root-caused.
  - **`ItemLost` "clause has no explanation" panic** (36%, 5/14) — an item gets consumed/lost but the belief's tracked exclusion set names only *other* items, none matching. 4/5 involve FocusSash specifically (excluded sets `[QuickClaw, ChoiceScarf]` or `[KingsRock, RazorFang]`); reads like one real mechanism recurring, not five coincidences. Not root-caused; next target if picking this up again.
  - **Leftovers item-Known-conflict** (7%, 1/14) — the family S53 mostly closed; down to noise level.
  - **NEW (2026-07-19): "truth ⊆ belief" subset oracle.** The failure rates above are
    all from the pre-existing oracle, which only asserts the belief never becomes
    self-contradictory — it says nothing about whether the belief has silently
    *narrowed past the true value* without ever going empty. Added a second,
    independent oracle (`information/subset_check.rs::assert_true_state_subset_of_belief`,
    wired into this same fuzz loop after every turn for both observers) that
    asserts the real concrete state is always a member of the set the belief
    admits — every hidden field's true value stays inside its bound, and every CNF
    predicate clause is satisfiable by the real assignment. A 100-iteration sweep
    (2026-07-19) measured a **36% failure rate (36/100)** — dramatically higher
    than the contradiction oracle's 0.14%, meaning the belief's *precision* has far
    more soundness gaps than its *consistency* does. Four overlapping families
    (bucketed qualitatively, not mutually exclusive — several failures show more
    than one symptom at once):
    - **Nature/stat-window over-narrowing** (~20/36, the dominant family). The
      belief's `possible_natures` and/or `min/max_stats` / `min/max_pre_nature_stat`
      windows exclude the Pokémon's real nature or real stat value outright (e.g.
      `nature: belief excludes true value Adamant`, `stats[2]: true=162 not in
      [108,133]`). Almost always co-occurs with the Pass 3/5 back-solve pipeline
      (`pass3_direction_a`/`pass3_direction_b`/`pass5_back_solve`) — the engine's
      own `[pass3-direction-* self-heal]` log lines fire constantly during the
      sweep, confirming this pipeline is already known to produce internally
      inconsistent bounds sometimes; self-heal only catches a NEW derivation that
      would cross an EXISTING bound, not an original bound that was simply
      computed wrong from the start (which is what this family shows: the
      violating bound is often the *first* one narrowing that field, no crossing
      event visible).
      **ONE root cause found and FIXED (2026-07-19, S56)**: the "genuine engine
      bug, not an oracle bug" suspicion above was confirmed and traced —
      `poke_rust/src/state/pokemon.rs`'s teamsheet `EVs:` line parser inline-scaled
      stat points via `--stat-points`'s `8p-4` formula, then unconditionally
      passed the ALREADY-scaled value into `build_pokemon_state`, which scales
      the same value a SECOND time. The second pass overflows `u8` and silently
      wraps mod 256 (e.g. 20 points → 156 → `(156*8-4) mod 256 = 220`), producing
      a bogus final stat with no error. Hand-verified byte-exact against all 4 of
      the Skeledirge instance's non-zero EVs via a temporary ground-truth dump;
      confirmed `calc_stat` on the wrapped EV=220 reproduces the exact reported
      true Def (162) that the belief's (CORRECT, singly-scaled) bound excluded.
      Fixed by removing the redundant inline scaling in the line parser (single
      point of scaling is now `build_pokemon_state` only). Regression:
      `teamsheet_stat_points_ev_scaling::ev_points_are_scaled_exactly_once`
      (`simulator_tests.rs`), verified red-without-fix/green-with-fix.
      **This is a core engine correctness bug, not just a fuzz-test artifact** —
      it corrupted the TRUE stats of any teamsheet-loaded Pokémon with non-zero
      EVs under `--stat-points` (the project default), in real battles too, not
      only this fuzz test.
      **Family NOT closed**: a follow-up 300-iteration sweep post-fix measured
      **37% (111/300)**, statistically unchanged from the pre-fix ~36%/100 —
      this bug was real and is now fixed, but is NOT the dominant cause of this
      family; at least one more, still-unfound cause remains in the Pass 3/5
      back-solve pipeline itself. Confirmed via hand-verification for one
      instance (Kingambit, `MA_venusaur_aerodactl.txt`) that pre-dates this fix —
      pre-nature Def BSV recomputed as 155 (base 120, IV 31, EV 116 post
      `--stat-points` scaling, level 50) against a derived bound of ≤143 — note
      EV 116 there is a *legitimately single-scaled* value (not itself an
      instance of the S56 double-scaling bug, since 116 is achievable via a
      single 8p-4 pass with p=15), so Kingambit's case is a DIFFERENT,
      not-yet-root-caused cause within this same family. Next step if picked up
      again: repeat this session's methodology (temporary `[P3A-TRACE]`/
      `[P3B-TRACE]` instrumentation in `pass3_direction_a`/`pass3_direction_b` at
      their `apply_unconditional_tightening` call sites, cross-referenced against
      a `[TRUTH-DUMP]` of the violating mon's raw stats in `subset_check.rs`,
      run via the `survey_subset_violations` pattern — a `catch_unwind`-based
      sweep harness that buckets failures across many iterations without
      aborting on the first one; NOT reproducible by seed, since
      `sample_turn_raw`'s internal branch sampling uses `rand::thread_rng()`
      rather than the test's own seeded RNG).
    - **Def-specific 3-literal clause violations** (~7/36) — clauses of the exact
      shape `[NatureBoostsStat{Def}, NatureNerfsStat{Def}, EVIVStatGE|LE{Def,
      value}]`, all unsatisfiable by the true nature+BSV. This is
      `emit_nature_conditional_bounds`'s neutral-nature-class branch
      (`information/inference.rs`) — very likely the SAME root mechanism as the
      family above (the per-nature-class BSV bound derivation it emits), just
      caught here as a predicate-clause violation instead of a direct field-bound
      violation. Disproportionately `Def` in this sample; unclear yet whether
      that's a real pattern or sampling noise.
      **Investigated further (2026-07-19, session 13, phase 2 of a 3-phase plan) —
      STILL NOT ROOT-CAUSED, but substantially narrowed.** Reproduced the exact
      Kingambit repro from the S56 entry above (`MA_venusaur_aerodactl.txt` vs
      `MA_charizard_sylveon.txt`, iter=5 turn=6) with fresh instrumentation
      (`[NATURE-CLASS-TRACE]` inside `emit_nature_conditional_bounds`, dumping
      each `NatureClassBound`'s `bsv_lo_neutral`/`bsv_hi_neutral`; a
      `[CLAUSE-TRUTH-DUMP]` at the clause-unsatisfiable panic site in
      `subset_check.rs`, extracting every `mon_idx` a failing clause references
      and dumping that mon's raw ground truth). Confirmed byte-exact: true
      Kingambit is `Adamant` (Def-neutral), `evs=[252,124,116,0,0,20]` (Def
      EV=116, a legitimately single-scaled value, NOT an S56 instance — see the
      family-1 entry above), true pre-nature Def BSV=150, post-nature (neutral
      mult=1.0) stat=155 (base 120). The derivation that produced the panic's
      `EVIVStatLE{Def,143}` clause came from `Ariados`'s `Knock Off` hitting
      Kingambit for only `pre_pct=100 → post_pct=93` (~7% of max HP, itemless
      Kingambit) — `[NATURE-CLASS-TRACE]` showed the **neutral class's own**
      `bsv_hi_neutral=143` for that exact hit (mod=1.0, i.e. this IS the true
      nature's class, and its own bound already excludes the true value before
      any cross-class union happens). **Ruled out**: Knock Off's target-item
      1.5× base-power boost — confirmed via `neutral_item()` (returns
      `Item::None` for an unresolved item field) that the neutral-gear search
      correctly assumes no item, matching Kingambit's real (itemless) state, so
      this is NOT a missing-modifier bug in the oracle's Knock Off handling.
      **Also confirmed**: `mon_violation`'s direct field check did NOT flag
      `stats[2]`/`max_stats[2]` for this same mon at this same moment (only the
      separate CNF clause panicked) — meaning the FIELD-level bound (fed by the
      union `global_stat_hi` across all three nature classes across the WHOLE
      game's accumulated hits) still correctly admitted 155, while the
      NEUTRAL-CLASS-SPECIFIC bound emitted by THIS one hit into the CNF clause
      did not. This confirms the two write sites (`apply_unconditional_tightening`
      → `min_stats`/`max_stats`, vs. `emit_nature_conditional_bounds` → the CNF
      clause) are tracking meaningfully different information across the game's
      full hit history, and the CNF clause's neutral-class value here is
      strictly tighter (and wrong) relative to what the field-level bound
      independently established. **Not yet root-caused**: didn't reach a
      conclusive explanation for why THIS SPECIFIC hit's neutral-class
      `find_feasible_bsv_range_a` search returns an upper bound (143) below the
      true value (150) for a small (~7%), non-crit, itemless, unboosted hit —
      the oracle's own damage formula, target types, and item assumption are
      all confirmed correct in isolation; the remaining suspects are (a) whether
      `achievable_defender_hp_values`/the per-hp_cand loop is missing Kingambit's
      TRUE max HP (207) as a tested candidate for THIS specific hit (not yet
      checked — `mon_violation` not flagging `stats[0]` only proves the FIELD's
      HP window is fine, not that this hit's own HP-candidate enumeration tested
      207), or (b) a rounding/truncation edge case specific to very-low observed
      damage magnitudes in `percent_delta_damage_band`/`bracketed_feasible_bsv_range`'s
      binary search when the true damage sits very close to a display-percent
      boundary. Next session: instrument `achievable_defender_hp_values` and the
      per-`hp_cand` loop directly (dump which HP values actually get tried for
      the Knock Off hit above) before re-deriving anything else — this is the
      one concrete branch not yet ruled in or out. Reuse `survey_subset_violations`
      (checked in permanently) plus the two trace patterns above (removed after
      this session, easy to re-add — see the exact code shape in this session's
      commit history if needed).
    - **Speed-order alternate-item escape clauses** (~7/36).
      **ROOT-CAUSED AND FIXED (2026-07-19, S57)** for the dominant instance of this
      family: a mon whose OWN move changes its OWN priority-lifting ability mid-move
      (Skill Swap being the clearest case — it swaps abilities between user and
      target, but Role Play/Entrainment/Trace/Mummy/Wandering Spirit/Simple Beam
      share the same shape) produced a `[SpeedComparison ∨ HasAbility(Prankster)]`
      escape clause that got silently discarded. Root cause was THREE compounding
      timing bugs, all in `information/inference.rs`:
      1. `pass4_speed_from_order`'s escape-disjunct construction (item/ability
         candidates for `fast_mon`/`slow_mon`) read the mon's CURRENT (live) belief
         state, not a turn-start snapshot — unlike every OTHER speed-relevant field
         in the same function (weather/terrain/boosts/paralysis/Tailwind), which
         already used `seed_state` specifically to avoid this class of bug. Fixed by
         preferring `seed_state` for the ability/item lookups too, guarded on
         `possible_mon_id` staying the same physical individual (falls back to live
         `state` if a mid-turn switch replaced the slot's occupant, since then
         `seed_state` describes the wrong mon).
      2. `EventKind::AbilityRevealed`'s handler overwrote `possible_abilities`
         directly on a live ability change (Skill Swap etc.) with no resolution step
         first — the ability-equivalent of the already-fixed S19 `HasItem`
         staleness bug, just never extended to abilities. Fixed by adding
         `resolve_ability_clauses_on_ability_change` (mirrors
         `resolve_item_clauses_on_item_change` exactly), called before the
         overwrite.
      3. Even with both fixes, Pass 4's SECOND call (a deliberate re-derivation
         after the event walk, to pick up newly-`Known` priority abilities) still
         reconstructed the pairing's clause AFTER the ability-change event had
         already run — using the historically-correct (seed_state) ability, but
         handing BCP a BRAND NEW clause that BCP immediately re-evaluated against
         the NOW-current (already-changed) ability and discarded via its own
         `[bcp self-heal] discarding unsatisfiable clause` path, since fix #2's
         resolution had already run and moved on before this new clause existed.
         Fixed by skipping clause (re-)construction entirely for a pairing when
         either mon's ability is `Known` to a value `seed_state` couldn't have
         admitted (a live-change signature, not mere narrowing) — the first call's
         clause, together with fix #2's resolution at the moment of change, already
         captured the truth soundly; nothing legitimate is lost by not re-deriving.
      Confirmed via a fuzz-oracle repro (`MB_gallade_clefable.txt`'s Sableye,
      Ability: Prankster, knows Skill Swap) that reliably hit exactly this dead
      clause. Regression: `test_s57_skill_swap_does_not_lose_its_own_prankster_escape`
      (`inference_tests.rs`), verified red-without-fix/green-with-fix. Re-swept:
      **28% (84/300)**, down from the ~36-37% baseline — clause-shape diversity in
      the survey harness's own bucketing collapsed from 6-9 distinct shapes to 2
      (each a single occurrence), consistent with this family being mostly closed.
      **Still open**: the `[HasItem{KingsRock}, HasItem{RazorFang}]`/flinch-clause
      variant of this family (same item sets as the open `ItemLost` contradiction
      panic) was investigated in parallel (`pass2_flinch_holder_from_cant` already
      correctly guards against stale mid-turn-switch attribution via
      `ctx.switched_slots_this_turn` — confirmed NOT the same bug as S57) but not
      root-caused; if picked up again, apply the SAME live-ability/item-change
      timing lens first (mirroring fixes #1-3 above) before assuming a fresh cause.
    - **Illusion/Zoroark disguise tracking** (~6/36, was flagged highest-priority
      to investigate given how central Illusion handling is to this engine's
      fog-of-war design). The primary belief entry (matching the *displayed*
      disguise species) AND its `possible_illusion_state` hypothesis both fail to
      admit the true, underlying disguised Pokémon — species, ability, weight, and
      all six stats simultaneously out of bounds. In more than one instance the
      belief had *already* resolved Zoroark's location to a different, fully
      `Known` roster slot (e.g. a bench mon) while the true Zoroark was actually
      active and disguised elsewhere — i.e. the belief committed to the wrong
      resolution.
      **RE-MEASURED (2026-07-19, session 13, phase 3 of the 3-phase plan) — rate
      dropped to ~0% as a side effect of S57, no code change needed.** Per this
      family's own hypothesis in the fix plan ("partly downstream of an unrelated
      over-narrowing triggering a false Zoroark promotion"), re-ran the
      permanently-checked-in `survey_subset_violations` harness TWICE (300
      iterations each, 600 total) after the S57 speed-order fix landed. **Zero**
      `ZoroarkHisui` (or any Illusion-capable species) failures in either sweep —
      down from consistently being the TOP or near-top offender in every pre-S57
      sweep this investigation ran (12/65, 19/300, 6/113, 5/113 across several
      runs). Deliberately did NOT touch `resolve_zoroark_globally`/
      `apply_with_illusion_mirroring`/`promote_illusion_to_primary` — per this
      project's own memory, self-healing or loosening the promotion signal there
      previously broke legitimate-detection tests
      (`test_zoroark_pass3_pass5_promotion_synergy`,
      `test_zoroark_learnset_promotes_when_primary_impossible`), and there is
      no evidence left of a live bug to justify that risk right now. **Not
      closing this TODO item outright** — 600 iterations is not proof of zero
      residual rate, and the underlying structural hazard (whichever mon's
      primary first goes infeasible gets promoted, even if that infeasibility
      came from an unrelated bug) is still real and undocumented in
      `information/README.md`. If this resurfaces after future over-narrowing
      fixes land (e.g. once the Def-clause family above is finally root-caused),
      re-sweep first before assuming a NEW Illusion-specific bug — it may just
      be another downstream symptom.
    - **Tested and ruled out as the primary driver**: whether an unrealistically
      wide item-possibility space was inflating the failure rate. The fuzz
      test's `InferenceConfig` previously left `legal_items: None` (~1,000 items
      possible) even though every checked-in teamsheet only ever holds the
      curated Champions catalog (`frontend/src/lib/items.ts`'s `CATALOG` —
      general items + Mega Stones + berries). Added `champions_legal_items()`
      (mirrors that catalog exactly) and wired it into the fuzz config
      permanently, matching what real battles actually run under. Re-swept:
      31/100 vs. the original 36/100 — a marginal, likely noise-level
      difference (the two 100-iteration runs aren't even a clean A/B — `sample_turn_raw`
      isn't seeded by the test's own RNG, so identical seeds don't replay
      identical trajectories run-to-run; see the methodology notes in memory
      `project_random_battle_fuzz_test_findings.md`). Kept anyway as a more
      realistic default, but the four families above are NOT primarily caused
      by item-pool width.
    - **Test suite structure**: because the subset-check families above are
      real, unfixed, and hit at a much higher rate than the contradiction
      oracle's historical 0.14%, wiring the check into the default (non-`#[ignore]`d)
      `random_doubles_battles_are_sound` — as originally done — made ordinary
      `cargo test` runs fail on unrelated work roughly a third of the time.
      Split into two tests instead: `random_doubles_battles_are_sound` (default,
      contradiction oracle only, matches its historical reliability) and
      `random_doubles_beliefs_stay_sound_subset` (`#[ignore]`d, both oracles —
      run explicitly with `cargo test -- --ignored` when working on the
      fog-of-war engine, or a temporary wider sweep as above for frequency data).
      Fold the subset check back into the default test once the four families
      are fixed.
### New features
- Link all the READMEs to the main project README.
- Frontend features
  - We need a favicon lul
  - Add benchmarking to the FE somewhere (right side of navbar?)
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
