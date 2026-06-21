# TODO

---
## Saved for later (Information only)
- Illusion — enter disguised as the last party member
- Anticipation — signal if opponents have SE or OHKO moves (message-only; no battle-state change in a full-information sim)
- Frisk — reveal an opponent's held item (message-only; no battle-state change in a full-information sim)

### Inference engine: deferred mechanics (`apply_information` — pass 3 and beyond)
Soundness is preserved by NOT narrowing on these until they are implemented.

**(A) Deferred — not yet inferred:**
- **Pass 3: damage→stat bounds** — currently stubbed. Requires tracking each mon's HP before and after
  each hit across the turn. Single-hit Physical/Special standard moves are the target; multi-hit,
  variable-BP, fixed-damage, OHKO, and Counter-class moves should be skipped (emit no bound).
- Multi-hit move damage (Bullet Seed, etc.) — variable total; skip narrowing.
- Variable-BP moves (Gyro Ball, Electro Ball, Low Kick, Heavy Slam, Heat Crash, etc.) — BP depends
  on hidden stats; skip narrowing until weight/speed inference is added.
- Fixed-damage / OHKO / Counter-Mirror-Coat class — no stat info; skip.
- Learnset-based Illusion narrowing — needs Showdown learnset data + parser (like `parse_pokemon_dex`).
  Drop Zoroark/ZoroarkHisui from `possible_species` when the observed move is illegal for that forme.
- Global EV total-cap exploitation — if stat-points has a 510-analogue cap, the total cap enables
  extra tightening across stats; leave as future enhancement.

**(B) Future inference sources (sound tightening opportunities, not yet implemented):**
- PP-based move-set inference (Pressure interactions, max-PP reveals from PP Ups).
- Status/secondary-effect absence revealing items/abilities (Shield Dust, no flinch → Inner Focus or
  King's Rock absent, etc.).
- Priority-move reveals implying abilities (Prankster / Gale Wings / Triage) and resulting bracket
  re-derivation feeding Pass 4.
- Contact-reaction items/abilities (Rocky Helmet / Rough Skin / Iron Barbs / Static / Flame Body /
  Poison Point chip revealing the defender's item or ability).
- Healing-amount reveals: Leftovers → `floor(maxHP/16)` exact fraction → HP-stat bound; berry heal
  fractions; Black Sludge discrimination between Poison and non-Poison holders.
- Weight-based move damage (Low Kick / Heavy Slam) cross-checking `possible_weight_hg`.
- Tera / forme reveals pinning `possible_tera_type` and ability sets further.
- Trick / Switcheroo / Knock Off chains — transfer known items between mons.
- Move-legality (learnset) constraints on `possible_species` beyond Illusion.
- Ability activation sub-priority — currently switch-in abilities are ordered purely by effective
  speed; revisit if Champions adds a sub-priority layer (this is a fix, currently unsound).

## Refactors
- Hidden information stuff, adding information releases to simulate_turn on a flag input
- Comments Deslop
