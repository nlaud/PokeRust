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
