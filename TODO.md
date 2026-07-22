# TODO: Always remove items from here when they are completed :)

### Fixes
- Cache the benchmarking so that it stays even when you switch tabs until you re-run them.
- Favorited teams should also be auto-selected first in the simulate / tracker flow.
- Weather turns are not accurately tracked by the tracker. volatiles side conditions, pseudoweathers, and weathers etc. should use their default time amounts, AND be decremented on endofturn
- Combine leads into one event, and have that allow reactions (i.e. p leads tyranitar lycanroc o leads aerodactyl charizard p1 sandstream o1 unnerve)
- Bug: protect does not automatically get the protected itself volatile??? ALL VOLATILES SHOULD BE TRACKED. ALL SECONDAR EFFECTS SHOULD BE TRACKED. GO THROUGH THE SIMULATOR, search the entire thing so ALL EFFECTS ARE PRESENT!!!!! 
- Bug: Mega abilities are not resolving properly: P2's Charizard Mega Evolved into Charizard-Mega-Y!

P2's Charizard-Mega-Y's Drought!

P1's Tyranitar Mega Evolved into Tyranitar-Mega!

P1's Tyranitar-Mega's Sand Stream! (No weather changes happening here, but should also work with intimidate and other abilities etc)
- I want this to accurately track battles, so if I have a simulated run and manually input the events that are happening into the tracker then it should be accurately tracking the state of the game. (MAKE A FUZZ TEST FOR THIS, same subsetting logic etc, since it should already be using the same unknownstate stuff!) 
- Weather turn information is leaked, it should display a range of the possible weather turns, same thing for other effect turns like reflect!!!!!! THERE SHOULD BE NO DEPENDENCE ON THE ACTUAL STATE FOR THE SIMULATOR, THIS SHOULD ALSO APPLY TO THE TRACKER!
- Future Sight / Wish (and Doom Desire / Healing Wish / Lunar Dance / Revival Blessing) have no tracker grammar at all — `SlotCondition`/`SlotConditionStart`/`SlotConditionEnd` exist as event kinds but `tracker_parse.rs` has zero handling for them, so a user can't even type these moves manually today. The hard part: `SlotCondition::FutureMove` snapshots the attacker's raw stats/boosts/ability/type at cast time, which the tracker's fog-of-war belief may not know precisely for an opponent's mon under Closed Team Sheet — needs a real design (bound/infer the snapshot the same way the inference engine bounds other hidden stats, or require the user to supply it) before adding grammar, not just a quick word mapping.

### New features

- Test the meta sampler & make sure a chunk of the data looks good...
  - Now on the Rust end, create a determinizer that takes in the meta state (we will need a parser for this) + an unknownbattlestate, then, like the simulator, has modes for giving a single random sample state or giving every possible state with probability (This should not consider "other" options, just the normal ones, it should force based on the meta percent and known information items, moves, etc.). It should output complete full states though that should just be able to be put in the simulator and work!
- Eventually create nash solver and recursive evaluation (When both players have perfect information)
  - [ ] Then move on to actual bot creation, battle and mentor pages (Could make these an option in the simulate / tracker page?).
