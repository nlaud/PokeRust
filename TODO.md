# TODO

---
## Information Abilities
- Illusion — enter disguised as the last party member
- Anticipation — signal if opponents have SE or OHKO moves (message-only; no battle-state change in a full-information sim)
- Frisk — reveal an opponent's held item (message-only; no battle-state change in a full-information sim)

## Refactors
- Unique Items Claude fo inference :)
- Hidden information stuff, adding information releases to simulate_turn on a flag input (is there a better way to do this that isn't just copying the entire function?)
- Comments Deslop
- MAKE THE FRONTEND YIPPEE
  - Start with Teams, Simulate, and Tracker pages
    - For tracker will need a parser for lines of input -> action / reaction tree (figuring out what is a reaction to what and what causes what from just the lines, also must add guaranteed effect so the user doesn't need to put those in manually). THis will likely include some RegEx stuff as well. Need a detailed spec.
    - https://stitch.withgoogle.com/projects/6512361286860616575 for designs
    - https://github.com/PokeAPI/sprites for FE sprites
  - Then move on to actual bot creation, battle and mentor pages.
