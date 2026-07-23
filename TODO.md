# TODO: Always remove items from here when they are completed :)

### Fixes
- Already a valid token checking for not displaying suggestions does not work for like hp percents and stuff like that
- Shift escape should not restart the turn, it should reset you to editing the end of the last turn.
- Also the behavior for submitting an event that is not the last event should be changed so it adds a new event after the one you were editing

### New features

- Test the meta sampler & make sure a chunk of the data looks good...
  - Now on the Rust end, create a determinizer that takes in the meta state (we will need a parser for this) + an unknownbattlestate, then, like the simulator, has modes for giving a single random sample state or giving every possible state with probability (This should not consider "other" options, just the normal ones, it should force based on the meta percent and known information items, moves, etc.). It should output complete full states though that should just be able to be put in the simulator and work!
- Eventually create nash solver and recursive evaluation (When both players have perfect information)
  - [ ] Then move on to actual bot creation, battle and mentor pages (Could make these an option in the simulate / tracker page?).
