# USAGE
cargo run -- --p1 ./teamsheets/{teamsheet path} --p2 ./teamsheets/{teamsheet path} -v 3

# TODO

### Fixes

### New features
- Create a function to take in battle state and battle actions, then apply those to create a vector of tuples possible battle states resulting from that along with their probabilities.
    - Later focus SLOWLY on implementing actual battle functions
        - Update Multiturn moves (Fly, phantom force, etc.)
        - Implement Sky drop effect + flinching
        - Status effects + chance effects
        - Follow Me, Helping Hand
        - Other Volatiles + Statuses
        - Trick Room + Tailwind
        - Other battlefield effects (weather + rooms + terrains)
        - Weather Ball
        - Berries, Unnerve?, Other Consumables (sash), Type Boost Items
        - Leftovers, misc. items (bright powder?)
        - Choice Items
        - Other significant abilities (Prankster, sturdy, pixilate + dragonize, spicy spray, shadow tag, clear body, huge power, zero to hero, scrappy, parental bond, intimidate, competetive + defiant, mega sol, mega launcher, levitate, telepathy, cloud nine, multiscale, stamina, mold breaker)
        - Moves with unique effects (explosion, encore, disable, etc..)
        - Unique Moves (Skill swap, Sky Drop, etc.)
        - Implement Struggling when no PP
        - Imperfect information handling
- Eventually create nash solver and recursive evaluation

### Random Stuff
- Make Pokemon Brought and Active a CLI argument
- Add a CLI flag for whether simulating user can choose which outcome happens.