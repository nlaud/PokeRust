# TODO: Always remove items from here when they are completed :)

### Fixes
- If a known mon starts weather, then the turns of weather should be fixed
- the autofill should match whatever casing the user has used in the line so far, defaulting to PascalCase if no two word things have shown up. It should only autofill to the casings that the grammar supports (not rock slide instead rock_slide or rock-slide etc)
- you should not be able to up arrow into the previous turn, also have a line number indicator at the beginning of the text bar
- autocorrect should not show up if the current word is a valid word
- Mega evolution seems to be broken: I'm getting Line 5: mega requires a species — this slot's species isn't known yet? or also I get that y is not a known species. But there's no zoroark its just tyranitar raichu team vs charizard aerodactyl team.
- There is some really strange behavior with slots not knowing their pokemon?? "P2 slot 2 took damage (now 0%)" should be aerodactyl took damage. When sending out it should be made clear the order of the slots.
- I thought we discussed this but there should be a new grammar for leads, so it would be leads p tyranitar lycanroc o charizard aerodactyl
- Make the autofill suggestions show diverse options some how, like instead of alphabetical have it be randomized alphabetical but stable somehow. I want if something is there it should stay if it is still an option, but I want to show a variety of different options not just always p, p1, p2, o, o1, o2.
- Clicking enter on an empty text box should just delete that event, same thing for backspace on an empty event

### New features

- Test the meta sampler & make sure a chunk of the data looks good...
  - Now on the Rust end, create a determinizer that takes in the meta state (we will need a parser for this) + an unknownbattlestate, then, like the simulator, has modes for giving a single random sample state or giving every possible state with probability (This should not consider "other" options, just the normal ones, it should force based on the meta percent and known information items, moves, etc.). It should output complete full states though that should just be able to be put in the simulator and work!
- Eventually create nash solver and recursive evaluation (When both players have perfect information)
  - [ ] Then move on to actual bot creation, battle and mentor pages (Could make these an option in the simulate / tracker page?).
