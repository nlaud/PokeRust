# Fog-of-war inference

The `information` module tracks facts that one player can observe.
It converts battle events into a sound belief about hidden opponent data.

Read this document before you change the module.

## Main rule

The belief must include every hidden value that can explain the observations.
Inference can keep an impossible value.
Inference must never remove a possible true value.

When several explanations remain, keep their union.

`apply_information` reports a contradiction when no explanation remains.

## Module map

| File | Purpose |
|---|---|
| `information.rs` | Defines observable events |
| `unknowns.rs` | Defines beliefs and logical statements |
| `inference.rs` | Applies the six inference passes |
| `inference/bcp.rs` | Propagates Boolean constraints |
| `materialize.rs` | Builds a damage-calculation state |
| `determinize.rs` | Samples one complete playable state |
| `subset_check.rs` | Checks belief soundness |
| `cps.rs` | Samples fixed-size move sets |
| `compositions.rs` | Samples bounded stat spreads |
| `describe.rs` | Converts beliefs to display text |

## Observable events

`InformationEvent` contains an `EventKind` and its reactions.

```text
InformationEvent
  kind
  reactions[]
    InformationEvent
      kind
      reactions[]
```

The tree records cause and effect.
For example, a move can cause damage.
The damage can reveal an item.
The item can cause recoil.

Do not flatten this tree.
Some inference rules depend on the parent event.

### Event order

The simulator supplies events in visible battle order.
`apply_information` does not reorder them.

The caller must preserve:

- Action order.
- Reaction order.
- End-of-turn order.
- Switch and entry-effect order.

### HP visibility

`PokemonHP` has two forms:

- `Number(u16)` stores exact HP.
- `Percent(u8)` stores visible percentage HP.

A player knows exact HP for its own Pokémon.
The player usually knows only percentage HP for an opponent.

Do not convert opponent percentage HP to one exact number.
Keep every exact value that can produce the percentage.

### Event groups

`EventKind` covers these groups:

- Moves and switches.
- Mega Evolution and Terastallization.
- HP changes and fainting.
- Hits, misses, immunity, and protection.
- Major status and volatile status.
- Stat-stage changes.
- Weather and terrain.
- Side and slot conditions.
- Item changes.
- Ability reveals.
- Form and identity changes.
- End-of-turn processing.

Read each variant's doc comment before you add an inference rule.
The comment defines the event contract.

When you add a variant, update these consumers:

1. The event mapper in `server/mapping.rs`.
2. The TypeScript DTO in `frontend/src/api/types.ts`.
3. The text renderer in `frontend/src/lib/eventText.ts`.

## Belief state

`UnknownMatchState` mirrors `MatchState`.
`UnknownBattleState` mirrors `BattleState`.
`UnknownPokemonState` mirrors `PokemonState`.

The belief stores known values, possible values, exclusions, and bounds.

### `Unknown<T>`

`Unknown<T>` forms a small knowledge lattice:

| Form | Meaning |
|---|---|
| `Known(x)` | Only `x` is possible |
| `Possibly(xs)` | One value in `xs` is possible |
| `Not(xs)` | Any value except a value in `xs` is possible |

Inference can narrow knowledge:

```text
Not([]) -> Not([x]) -> Possibly([a, b]) -> Known(a)
```

Inference must not widen knowledge during normal event processing.
Special identity changes can replace one field with a new valid domain.

Use one representation for one fact.
Do not store the same constraint in two fields without a synchronization rule.

### Pokémon fields

`UnknownPokemonState` tracks these hidden values:

- Species and possible disguise species.
- Ability and original ability.
- Held item.
- Moves.
- Nature.
- IV ranges.
- EV ranges.
- Stat ranges.
- Tera type.
- Mega form.
- HP.
- Status.
- Volatile status.
- Stat stages.

Exact values use `Unknown::Known`.
Numeric uncertainty usually uses minimum and maximum arrays.

### Stable identity

`mon_idx` identifies one physical Pokémon.
It does not identify an active slot.

Active and bench vectors can move a Pokémon.
The `mon_idx` must move with it.

Statements use `mon_idx`.
This keeps a statement attached after a switch.

Do not infer identity only from species.
Two teammates can have the same visible species through Illusion or form changes.

## Boolean statements

Some observations have several possible causes.
The belief stores those causes as clauses.

A clause is a disjunction:

```text
A or B or C
```

The full predicate list is a conjunction:

```text
clause_1 and clause_2 and clause_3
```

This is conjunctive normal form.

Common statements include:

- The Pokémon has an item.
- The Pokémon has an ability.
- The Pokémon knows a move.
- The Pokémon has a status.
- The nature raises or lowers a stat.
- A base stat value has a lower or upper bound.
- A field timer has one value.
- One Pokémon moved before another.
- A Pokémon knows a threatening move.

Relational statements do not set one field.
Keep them as statements until another fact resolves them.

## Inference pipeline

`apply_information` uses six passes.
The order is part of the design.

```text
events
  -> structural facts
  -> item and ability reasoning
  -> damage bounds
  -> speed bounds
  -> EV, IV, and nature bounds
  -> Boolean propagation
```

## Pass 1: structural facts

Pass 1 applies facts that an event states directly.

Examples:

- A move reveal fills a move slot.
- An item reveal sets the held item.
- An ability reveal sets the ability.
- A status event sets or clears status.
- A switch moves a Pokémon between active and bench storage.
- A damage event updates HP.
- A faint event moves a Pokémon to the fainted group.
- A form event updates visible identity.
- A field event updates weather, terrain, or conditions.

Pass 1 also maintains event-history fields.
Later passes use this history.

Do not add absence reasoning to Pass 1.
A missing reaction has meaning only after the full action tree is available.

The tracker preview runs a limited Pass 1.
It must not apply whole-turn absence rules.

## Pass 2: item and ability reasoning

Pass 2 uses visible effects and missing effects.

Examples:

- Life Orb recoil proves a Life Orb.
- Missing Life Orb recoil excludes a Life Orb when no other rule prevents recoil.
- Rocky Helmet damage proves a Rocky Helmet.
- Two different moves can exclude a Choice item.
- A weather change can prove a weather-setting ability.
- A missing entry effect can exclude an entry ability.
- An unexplained miss can add an evasion-item or evasion-ability clause.

Check every prevention rule before you infer from an absent effect.

Possible prevention rules include:

- Magic Guard.
- Protective Pads.
- Ability suppression.
- Item suppression.
- Substitute.
- Immunity.
- Maximum or minimum stat stages.
- Existing weather or terrain.
- A fainted target.

Use the event snapshot from the time of the interaction.
Do not use only the final state of the turn.

## Pass 3: damage bounds

Pass 3 converts observed damage into stat bounds.

It calls the simulator damage code as an oracle.
It does not copy the damage formula.

For each possible explanation, it searches attacker and defender stat values.
It keeps every value that can produce the observed damage.

The pass considers:

- Damage rolls.
- Critical hits.
- STAB.
- Type effectiveness.
- Weather and terrain.
- Stat stages.
- Items.
- Abilities.
- Variable move power.
- Multi-hit behavior.
- Percentage HP rounding.

The search has two directions:

- The observer attacks the opponent.
- The opponent attacks the observer.

The directions constrain different hidden stats.
Do not reuse one direction's bounds in the other direction.

`materialize.rs` builds the temporary state for this oracle.
That state is not a playable battle state.

## Pass 4: speed bounds

Pass 4 uses action order within one priority group.

It considers:

- Move priority.
- Effective Speed.
- Trick Room.
- Tailwind.
- Paralysis.
- Items and abilities.
- Speed stages.
- Random order effects.

A random order effect can prevent a direct speed bound.
Store a disjunction when either speed or the random effect can explain the order.

Do not compare actions from different priority groups.
Do not compare an action with a switch unless the engine rules permit that comparison.

## Pass 5: EV, IV, and nature bounds

Pass 5 converts stat bounds into training bounds.

It uses:

- Species base stats.
- Level.
- Nature modifiers.
- IV bounds.
- EV bounds.
- The stat formula.

It tests each legal nature.
It removes a nature only when no legal IV and EV values satisfy the stat range.

When `use_stat_points` is true, EV values use this lattice:

```text
0, 4, 12, 20, ..., 252
```

The authoring point conversion is:

```text
ev = max(0, 8p - 4)
```

The default total EV cap is 516.
Do not change it to 510.

Champions permits 66 authoring points.
Three nonzero stats can produce 516 scaled EVs.

Pass 5 can use minimum EVs in other stats to reduce one maximum EV.
The configured cap must be an upper bound for every legal build.

## Pass 6: Boolean propagation

Pass 6 runs Boolean constraint propagation to a fixed point.

For each clause:

1. Remove a statement that is false.
2. Remove a clause that already has a true statement.
3. Apply the last statement when only one unresolved statement remains.
4. Repeat after any field changes.

An empty unresolved clause is a contradiction.

Propagation can update items, abilities, moves, natures, stats, and timers.
An update can make another clause resolvable.

Keep propagation deterministic.
Do not depend on hash-map iteration order.

## Illusion

The belief uses parallel hypotheses for Illusion.

The visible Pokémon record keeps its visible species.
`possible_illusion_state` stores a possible Zoroark state.

Both hypotheses receive observations that apply before Illusion ends.
This includes HP, status, moves, items, and abilities when appropriate.

When Illusion ends:

1. Promote the Zoroark hypothesis.
2. Set the real species.
3. keep the accumulated visible state.
4. Remove a stale duplicate from the bench.
5. Remove statements that refer to the discarded identity.

Do not widen the visible species to include Zoroark.
That older model lost roster identity during switches.

An unresolved disguised Pokémon can switch out.
Move the full parallel hypothesis to the bench.

Learnset data can remove a disguise candidate.
Remove a species only when it cannot learn a revealed move.

## Materialization

`materialize.rs` creates a state for damage calculations.
It fills unknown fields with conservative values.

The result has these intentional limits:

- Empty benches.
- Zero move PP.
- Approximate HP.
- Separate stat overrides.
- No guarantee of legal commands.

Do not use this result with `simulate_turn`.
Do not repair these limits.
Pass 3 depends on them.

Use `determinize.rs` when you need a playable state.

## Determinization

The determinizer samples one complete state from a belief.
It uses competitive usage data when possible.

It copies the observer's side.
It samples only hidden opponent data.

The result must satisfy:

- Belief bounds.
- Revealed moves.
- Species and item clauses.
- Legal move sets.
- Legal stats.
- Legal bench size.
- Runnable battle commands.

The returned probability is the product of the sample choices.
It is a lower bound for that sampled state.
Compare it only with samples from the same belief.

## Configuration

`InferenceConfig` controls:

- Stat-point mode.
- Maximum IV mode.
- Default level.
- Legal items.
- Repeated items.
- Learnset data.
- The total EV cap.

Match the configuration to the battle format.
An incorrect legal domain can make sound inference impossible.

## Testing

Inference tests use these groups:

- Direct event tests.
- Regression tests named `test_s<NN>_*`.
- Round-trip tests named `roundtrip_s<NN>_*`.
- Determinizer subset checks.
- Random battle soundness tests.

A round-trip test follows this process:

1. Start with a concrete state.
2. Simulate visible events.
3. Apply the events to a belief.
4. Check that the concrete state remains inside the belief.

Add a regression test for every soundness bug.
Test the smallest event tree that reproduces the bug.

Run the inference tests:

```sh
cd poke_rust
cargo test inference
```

Run the ignored random soundness tests before a large inference change:

```sh
cargo test --release -- --ignored
```
