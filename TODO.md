# TODO
### Fixes
- [x] Zoroark switching: an incoming mon that had been consumed as a disguise decoy
  (or discarded after promotion) was rebuilt species-only via `from_opponent_species`,
  regressing a fully-known open-sheet mon to "no information." Fixed via
  `p{side}_roster_templates` (pristine team-preview snapshots) preferred by
  `restore_discarded_primary_to_bench` and `pass1_switch`'s fallback, plus
  `finish_illusion_promotion_restore` calling that restore at EVERY promotion site
  (not just the `IllusionEnded` handler) — a live server run showed promotion via
  move-legality mirroring can resolve a disguise well before it visibly breaks,
  which previously dropped the decoy from the roster forever. Un-revealed
  switch-out/return and doubles partner-switch round-trips (both singles and
  doubles) are covered by regression tests; see `information/README.md`'s
  "Illusion: the parallel-hypothesis model" section for the full design.
- Add a test that just does the simulator + inference engine for both players, with random teams from the teamsheets and just clicking random moves until one player wins (do this like 25 times). And make sure this can run consistently and pass, since there shouldn't be anything impossible happening.
### New features
- Link all the READMEs to the main project README.
- Frontend features
  - We need a favicon lul
- Implement Closed Team Sheets information mode
- Eventually create nash solver and recursive evaluation (When both players have perfect information)
- Create a meta sampler from pikalytics, and then get the algorithm to understand that
  - [ ] Tracker page: needs a parser for lines of input -> action / reaction tree
        (figuring out what is a reaction to what and what causes what from just the
        lines, also must add guaranteed effects so the user doesn't need to put those
        in manually). This will likely include some RegEx stuff as well. Need a
        detailed spec.
    - https://stitch.withgoogle.com/projects/6512361286860616575 for designs
    - https://github.com/PokeAPI/sprites for FE sprites (fetched at runtime, never committed)
  - [ ] Then move on to actual bot creation, battle and mentor pages.
