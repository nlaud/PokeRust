# TODO
### Fixes
- Another bug with: [inference contradiction] context=0 — SpeedComparison raises min(248) above max(195)
  - This one might be caused by tailwind? It has something to do with a fast pokemon (like aerodactyl) using tailwind?
  - Another bug with the same error where this does not take into account extra stats from mega evolution?
  - The inference stat calculation engine really needs work, there are many edge cases unaccounted for!
- Tailwind and weathers ending does not produce an event?
- Erros with the inference enging should also add the Event currently being resolved as text
- Even in imperfect information modes, exact HP values are shown... NOTHING should render based on the true state of the battle, it should all be from the inferred state.
  - This is also shown in the sidebar.
  - The renderer should literally not even have access to the actual state of the battle when the information mode is imperfect, except for choosing moves.
  - Additionally, zoroark's items, types, moves, and stats are shown when those should NOT be known. It should display the true range of possible stats for that pokemon, as well as the fact that the typing is unknown and the pokemon name should show milotic, zoroark. Finally, zoroark should show up as possibly in the back, which it currently doesnt when led!
    - Once zoroark is switched out, it is fully revealed but this 
  - When pokemon are switched in later on in the battle, information from the teamsheet about that pokemon is not used when it otherwise should be (information should be known, even if there are multiple of the same mon in the teamsheet, you can get SOME information, even if you don't know which it is).
- Figure out what causes predicates to show up?
- The damage-inference engine often finds no information at all when it runs. This isn't a soundness bug — the HP-vs-DEF-EV scenario and the `pass5-hp` "no IV/EV can produce observed HP bounds" panic were both investigated and fixed (S30, see the regression tests in `inference_tests.rs`) — but Pass 3/5's back-solve is still weak/imprecise in the common case. Worth revisiting for more inferential power without sacrificing soundness.
### New features
- Add inference to the README, link all the READMEs to the main project README.
- Frontend features
  - Cheeky crossfade between screens? (Also when starting a new battlke)
  - Need to display the number of back mons in parentheses next to the possibly in the back text.
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
