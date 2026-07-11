# TODO
### Fixes
- Mons are displayed as possible in the back only when not actually in the back, you shuoldn't be able to see what mons are actually in the back until they are revealed then switched out, while currently back mons immediatley show up!
- Switching between your oppoents tab and your tab adds random pokemon from your opponents team to your teams display? This has some weird behavior also like your opponents back mons having their dropdowns connected...
- Assumptions for damage are incorrectly assuming things about the HP and DEF stats, specifically sometimes it assumes you have defense EVs or whatever when you really just have HP evs, THIS IS A SOUNDNESS VIOLATION. THis system in general feels really buggy; most of the time it gets no infered information and when it does it finds things wrong. Consider larger changes for this system.
  - I'm also getting "thread 'tokio-rt-worker' (97912) panicked at src\information\inference.rs:7229:13:
  [inference contradiction] context="pass5-hp" — no IV/EV can produce observed HP bounds", presumably because of similar issues, possibly because the inference engine does not consider multiscale??
- Zoroark does NOT display properly (it displays the copied pokemons item, when it should display that the species could be the copied species or zoroark (all possible clones should ahve this, things that have already been damaged should have it removed, same thing for the item of all pokemon should be the item of the zoroark, with predicates tying them together, if the species is zoroark the nit has the zoroark item, otherwise the copied pokemons item.)). You should not be using any information from the actual team sheet for displaying the opponents data, just the possible state. 
- I randomly get "thread 'tokio-rt-worker' (102752) panicked at src\information\inference.rs:280:17:
[inference contradiction] context="ability-absence-weather" — exclude("ability-absence-weather") conflicts with Known value
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace

thread 'tokio-rt-worker' (98944) panicked at src\bin\server\routes.rs:184:44:
called `Result::unwrap()` on an `Err` value: PoisonError { .. }"
  - I think this has something to do with the fact that we changed how weather works, as in if a weather is currently present resetting the weather does not reset the duration to 5.
- Errors like this one should not immediately destroy the gateway. Instead it should display a red error on the frontend (like errors already show up), and the gateway should continue running using the most recent information as the source of truth.
### New features
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
