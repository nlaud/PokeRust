# USAGE
cargo run -- --p1 ./teamsheets/{teamsheet path} --p2 ./teamsheets/{teamsheet path} -v 3

# TODO

### Fixes

### New features
- Create a function to take in battle state and battle actions, then apply those to create a vector of tuples possible battle states resulting from that along with their probabilities.
    - Later focus SLOWLY on implementing actual battle functions
        - Crits Ignore Drops + whatnot
        - All moves do at least 1 damage (except splash)
        - Healing moves (fracitonal etc.)
        - Confusion
        - Other battlefield effects (rooms + terrains)
        - Weather Ball
        - Multi-hit moves, switch out moves
        - Berries, Unnerve?, Other Consumables (sash), Type Boost Items
        - Test cases for major features
        - Leftovers, misc. items (bright powder?)
        - Choice Items
        - Side conditions (Types of spikes)
        - Other significant abilities (Prankster, sturdy, pixilate + dragonize, spicy spray, shadow tag, clear body, huge power, zero to hero, scrappy, parental bond, intimidate, competetive + defiant, mega sol, mega launcher, levitate, telepathy, cloud nine, multiscale, stamina, mold breaker, disguise, technician, scrappy, compound eyes, friend guard)
        - Other significant items!
        - Moves with unique effects (explosion, encore, disable, skill swap, roost, burn up, body press, foul play, etc..)
        - Other Volatiles + Statuses
        - Implement Struggling when no PP
        - Imperfect information handling
- Eventually create nash solver and recursive evaluation

### Random Stuff
- Make Pokemon Brought and Active a CLI argument
- Add a CLI flag for whether simulating user can choose which outcome happens.
- Unit Tests...

### Resources
Sequencing: https://bulbapedia.bulbagarden.net/wiki/User:FIQ/Turn_sequence
Sequencing 2: https://www.smogon.com/forums/threads/sword-shield-battle-mechanics-research.3655528/page-64#post-9244179 