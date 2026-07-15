# TODO
### Fixes
- We need to rework how zoroark works with the inference engine from the ground up (when information mode is not perfect information). 
  - When a team has zoroark, all pokemon should be treated as if they could be zoroark. While they should be displayed as the original pokemon on the FE, their actual stats/moves/item should be treated as if they could be zoroark (It it should be displayed as if the possible moves are A Or B, where A is the original move and B is the zoroark move, at least in a teamsheet mode when you know the moves of each of your opponents mons), the same should be true for items, abiltiies, natures, and stat spreads should encompass both the original and zoroark stats (this should update once zoroark is found and the pokemon is either revealed to be zoroark or not). 
  - Prior to zoroark being revealed it should be listed as possibly in the back (as well as all pokemon being possibly in the back, unless there are two of that pokemon in the front, since this means both the original and zoroark are in the front). The types of pokemon should be displayed as their original types on the frontend, and the inference engine should handle both cases of zoroark and the original at all times, keeping the learnset based deduction and stuff.
    - This can somewhat easily be handled by having a predicate that states that all pokemon are either zoroark with zoroark's moves, abilities, etc. or the original pokemon with its moves, abilities, etc.
  - It is okay to assume that a pokemon that was in the front is in the back one it switched out, unless it is already on the field (there were originally two on the field). This is because either zoroark or the original pokemon could have been in the front, and either way there must have been the original in the back at some point (zoroark transformed into the back mon).
  - Remeber that we optimize for SOUNDNESS FIRST, so losing information is okay for the sake of soundness above all else
  - Tracking zoroark throughout the course of a battle is important and extremely difficult. I want this logic to be bulletproof, and I want the frontend to always display the information that player one would have, although when choosing moves for player 2 it should display information that player 2 would have. 
  - Also there is a bug with zoroark. While the moves should be unknown (A or B state) in the sidebar when you don't know if a pokemon is zoroark, when choosing moves it should still show the correct moves so selection goes properly.
- Add a test that just does the simulator + inference engine for both players, with random teams from the teamsheets and just clicking random moves until one player wins (do this like 25 times). And make sure this can run consistently and pass, since there shouldn't be anything impossible happening.
### New features
- Link all the READMEs to the main project README.
- Frontend features
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
