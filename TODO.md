# TODO
### Fixes
  - **NOT fixed — deferred, root cause identified.** "Zoroark should show up as
    possibly in the back when led" / "switching it out fully reveals it": traced to
    `UnknownTeamPreviewState::into_battle_state` (`unknowns.rs:421`), which builds the
    *leading* mon's belief entry directly from the team-preview roster at
    `p2_tp.active_indices` (the true physical slot) with zero illusion-widening — unlike
    `pass1_switch`'s mid-battle handling, there's no `maybe_widen_for_illusion`-equivalent
    call in the team-preview bootstrap. A leading, disguised Zoroark's belief is
    therefore `Known(Zoroark)` with its full real stats/moves/item from turn 0 (not
    leaked to the frontend today only because `pokemon_view`'s displayed species reads
    ground-truth `PokemonState.illusion_disguise`, a separate field from the belief —
    but the belief itself is wrong/overconfident, and the "possibly in the back" entry
    for that same physical mon never gets created). A correct fix needs to generalize
    the widening logic to the team-preview bootstrap AND reconcile it with the
    known-bench double-counting guards from S29 (`test_s29_ambiguous_disguise_discarded_on_switch_out`
    in `inference_tests.rs`) — the mid-battle switch case also currently leaves a
    disguise-eligible Zoroark's own benched entry sitting `Known` and untouched in
    `known_back` even while its disguise is active elsewhere, which is a related leak.
    This is meaningfully riskier than the fixes above (touches audited Illusion/S29
    machinery) and was deliberately not attempted without a live repro to verify against.
  - Teamsheet info reuse on switch-in — **research correction**: this already works via
    the existing bench consume/re-bench cycle (`pass1_switch`'s known-then-possible
    fallback, `bench_outgoing_mon`) — a switched-in mon correctly picks up its prior
    accumulated knowledge from the matching bench entry. The one gap is the Illusion
    case just above (`bench_outgoing_mon` discards rather than benches an unresolved
    disguise, S29) — not a general teamsheet-reuse bug.
- The damage-inference engine often finds no information at all when it runs. This isn't a soundness bug — the HP-vs-DEF-EV scenario and the `pass5-hp` "no IV/EV can produce observed HP bounds" panic were both investigated and fixed (S30, see the regression tests in `inference_tests.rs`) — but Pass 3/5's back-solve is still weak/imprecise in the common case. Worth revisiting for more inferential power without sacrificing soundness.
- Double check that simulator probabilities are being handled correctly, and that there are no rounding errors?
### New features
- Add inference to the README, link all the READMEs to the main project README.
- Frontend features
  - Cheeky crossfade between screens? (Also when starting a new battlke)
  - Need to display the number of back mons in parentheses next to the possibly in the back text.
  - Default tab for battle view should be "opponent"
  - Light screen and reflect should both be the color of current reflect
  - We need a favicon lul
- Implement Closed Team Sheets information mode
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
