# USAGE (within /poke_rust)
cargo run -- --p1 ../teamsheets/{teamsheet path} --p2 ../teamsheets/{teamsheet path} -v 3

# TODO

### Fixes

### New features
- Create a function to take in battle state and battle actions, then apply those to create a vector of tuples possible battle states resulting from that along with their probabilities.
    - Handling Imperfect information, how to input information and update imperfect information states
    - Update Simulator.md to output information that each player has
- Eventually create nash solver and recursive evaluation (When both players have perfect information)
- Create a meta sampler from pikalytics, and then get the algorithm to understand that

### Resources
Sequencing: https://bulbapedia.bulbagarden.net/wiki/User:FIQ/Turn_sequence
Sequencing 2: https://www.smogon.com/forums/threads/sword-shield-battle-mechanics-research.3655528/page-64#post-9244179
