# TODO

---
## Saved for later (Information only)
- Illusion — enter disguised as the last party member
- Anticipation — signal if opponents have SE or OHKO moves (message-only; no battle-state change in a full-information sim)
- Frisk — reveal an opponent's held item (message-only; no battle-state change in a full-information sim)

### Inference engine: deferred mechanics (`apply_information` — pass 3 and beyond)

**(B) Future inference sources (sound tightening opportunities, not yet implemented):**
- Ability inference by species
- Status/secondary-effect absence revealing items/abilities (Shield Dust, no flinch → Inner Focus or
  King's Rock absent, etc.).
- Priority-move reveals implying abilities (Prankster / Gale Wings / Triage) and resulting bracket
  re-derivation feeding Pass 4.
- Contact-reaction items/abilities (Rocky Helmet / Rough Skin / Iron Barbs / Static / Flame Body /
  Poison Point chip revealing the defender's item or ability).
- Healing-amount reveals: Leftovers → `floor(maxHP/16)` exact fraction → HP-stat bound; berry heal
  fractions; Black Sludge discrimination between Poison and non-Poison holders.
- Trick / Switcheroo / Knock Off chains — transfer known items between mons.
- Ability activation sub-priority — currently switch-in abilities are ordered purely by effective
  speed; revisit if Champions adds a sub-priority layer (this is a fix, currently unsound).

## Refactors
- Hidden information stuff, adding information releases to simulate_turn on a flag input
- Comments Deslop
- https://github.com/PokeAPI/sprites for FE sprites
