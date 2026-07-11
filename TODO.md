# TODO
### Fixes
  - **[FIXED]** "Zoroark should show up as possibly in the back when led" / "switching
    it out fully reveals it": root cause was `UnknownTeamPreviewState::into_battle_state`
    (`unknowns.rs:421`) building the *leading* mon's belief entry directly from the
    team-preview roster at the TRUE physical active index — ground truth the belief has
    no business knowing, since the observer only ever sees what's DISPLAYED, which can
    be a disguise. Fixed by changing `into_battle_state` to park P2's entire roster
    (active pick included) in `possible_back` with `p2_active_mons` starting empty, and
    changing `session.rs::resolve_turn` to run `apply_information` over the team-preview
    transition's own event log immediately afterward (previously skipped entirely for
    this transition) — that log's `SimultaneousSwitch` already carries each lead's
    perspective-gated DISPLAYED species (`compute_illusion_disguise`,
    `simulator/mod.rs::battle_state_from_preview_branching`), so Pass 1's existing
    switch-in handling (`pass1_switch`, shared verbatim with every mid-battle switch)
    matches it against the roster and runs the SAME Illusion widening
    (`maybe_widen_for_illusion`/`widen_item_for_illusion`) a mid-battle disguised
    switch-in already gets — no new Illusion-specific logic needed, just routing team
    preview through the existing mechanism. Verified both empirically (all S29/Illusion
    regression tests + full suite pass, 1263/1263) and via two new tests:
    `test_disguised_lead_widens_to_possibly_zoroark_and_stays_in_possible_back` (the
    exact TODO repro — a disguised lead widens to `Possibly([shown, Zoroark])` and
    Zoroark's own roster entry survives in `possible_back`) and
    `test_into_battle_state_then_apply_information_places_lead_active` (proves the new
    two-step flow is byte-identical to the old one-step behavior for a normal,
    non-disguised lead).
- Getting errors like: "[inference contradiction] context=0 event=VolatileEnd { target: P1_1, volatile: RagePowder } — SpeedComparison raises min(148) above max(99)"
  - Obviously volatiles ending should not determine speed order since it isn't related to speed order at all
  - Speed order should not be considered at end of turn in general
- Also sometimes errors like "[inference contradiction] context=2 event=EndOfTurn — SpeedComparison raises min(198) above max(112)" happen when I use trick room?, but strangely only sometimes, like when opponents use protect?
  - I think this warrants a deep dive into the simulator, figuring out in all cases what the ordering for events is and which should be used for speed comparison in the inference
  - Something causing this: Team Preview
  P2 sent out Aerodactyl (100%)
  
  P1 sent out Lycanroc (150 HP)
  
  P2 sent out Charizard (100%)
  
  P1 sent out Tyranitar (203 HP)
  
  P2's Aerodactyl's Unnerve!
  
  P1's Tyranitar's Sand Stream!
  
  The weather became Sandstorm!
  
  Turn 1
  P2's Aerodactyl used Protect!
  
  P2's Aerodactyl protected itself!
  
  P2's Charizard used Protect!
  
  P2's Charizard protected itself!
  
  P1's Tyranitar used Protect!
  
  P1's Tyranitar protected itself!
  
  P1's Lycanroc used Rock Slide!
  
  P2's Aerodactyl blocked the attack!
  
  P2's Charizard blocked the attack!
  
  End of turn
  
P2's Charizard took damage (now 94%), when I seleect Rock slide for tyranitar and lycanroc, then tailwind and heat wave"[inference contradiction] context="pass5-hp" event=EndOfTurn — no IV/EV can produce observed HP bounds", maybe it is a good idea to just supress everything end of turn, except maybe perish events, since those do happen in speed order (check this with the simulator)
- Weather speed stuff is not dynamically updated. You need to make everything relating to speed tiers, damage claculation, and all that update dynamically as you progress through the turn.
  - There is currently a bug where if you use tailwind and heat wave, the speed tiers updating does not work properly because it sees that teh charizard is faster than the aerodactyl after the tailwind, but obviously it should not be (the charizard is slower before the tailwind, so the tailwind goes first). These speed cases should be calculated sequentially. Figure out the fastest move, then do all the derivations relating to that move being first (all pokemon that have not moved yet must be slower). Then resolve the effects of that move and update everything (trick room, sand rush, tailwind, etc.). Might be best to intertwine this with the state inference logic, but if it is sinificantly easier to leave as is then we should do that.
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
