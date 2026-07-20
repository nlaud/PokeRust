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

### Documentation map

- [`frontend/README.md`](frontend/README.md) — web UI: running the dev server,
  architecture, and frontend-specific mechanics notes.
- [`poke_rust/src/information/README.md`](poke_rust/src/information/README.md) —
  design doc for the fog-of-war inference engine (event vocabulary, the
  `Unknown<T>` lattice, the six-pass pipeline).
- [`meta_scraper/README.md`](meta_scraper/README.md) — Python tool that caches
  Pokémon Champions competitive usage stats from championsbattledata.com.
- [`poke_rust/benches/RESULTS.md`](poke_rust/benches/RESULTS.md) — recorded
  turn-resolution benchmark results and analysis.
