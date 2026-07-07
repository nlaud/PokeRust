# USAGE

### CLI (within /poke_rust)
cargo run -- --p1 ../teamsheets/{teamsheet path} --p2 ../teamsheets/{teamsheet path} -v 3

### Web UI
Run the API server (`cd poke_rust && cargo run --release --bin server`) and the
frontend dev server (`cd frontend && npm run dev`), then open http://localhost:5173.
See `frontend/README.md` for details.

### Benchmarks
Turn-resolution speed across enumerate/sample mode, damage-roll counts, and
crit branching (singles and doubles scenarios):

    cd poke_rust
    cargo bench --bench turn_speed

Takes a couple of minutes (the tractable doubles enumeration rows run for
seconds each; intractable ones are skipped). Recorded results and analysis
live in `poke_rust/benches/RESULTS.md` — append a new section there when
re-measuring after engine changes.

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
