# TODO
### Fixes
- Unseen fist not working??: P1 sent out Gengar-Mega (147 HP)

P1's Ninetales-Alola used Protect!

P1's Ninetales-Alola protected itself!

P1's Gengar-Mega used Protect!

P1's Gengar-Mega protected itself!

P2's Golurk-Mega used Headlong Rush!

P1's Gengar-Mega blocked the attack! (Mega Golurk has unseen fist? This also probbably needs inference fixing so that the unseen fist protect 1/4 gets accounted for...)
- "P2's Incineroar was cured of put to sleep!" Should say woke up not cured of put to sleep lol
- You can see Choice Lock volatile on your opponents mons even when you don't have enough information to be able to know they are choice locked. Fix this. Yous hould only be able to see this volatile (check for other volatiles that could be hidden / unknown) when you can guarantee its state.
- Should not display Not of items that are not even in the current format!
- `random_doubles_battles_are_sound` (poke_rust/src/tests/random_battle_tests.rs): fog-of-war soundness fuzz test. Long fix history (S1–S54) in memory `project_random_battle_fuzz_test_findings.md` — not reproduced here. A 10,000-battle sweep (2026-07-18) measured the current failure rate at **0.14% (14/10,000)**, split across three open families:
  - **pass5 "every candidate nature is infeasible" panic** (57% of failures, 8/14). Some derivation narrowed a mon's stat window past what any nature/IV/EV combo can produce. Confirmed NOT caused by the already-fixed S54 mechanism (HP-dependent base power) — stats/moves vary each instance (Spe, SpA, SpD; LastRespects, IcyWind, ExtremeSpeed, FlipTurn, MegaEvolution turns), no single pattern yet. Not root-caused.
  - **`ItemLost` "clause has no explanation" panic** (36%, 5/14) — an item gets consumed/lost but the belief's tracked exclusion set names only *other* items, none matching. 4/5 involve FocusSash specifically (excluded sets `[QuickClaw, ChoiceScarf]` or `[KingsRock, RazorFang]`); reads like one real mechanism recurring, not five coincidences. Not root-caused; next target if picking this up again.
  - **Leftovers item-Known-conflict** (7%, 1/14) — the family S53 mostly closed; down to noise level.
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
