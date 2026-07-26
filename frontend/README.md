# PokeRust Frontend

Minimalist web UI for the PokeRust battle simulator: a **Teams** page
(Showdown-format teamsheets in localStorage, the default route), a **Formats**
page (ruleset cards with a curated Pokémon Champions item pool for ban lists),
a **Simulate** page (hotseat battles against the Rust engine), a **Tracker**
page (follow a real battle by typing what happened instead of driving a
simulated opponent — see "Tracker mode" below), and a **Benchmark** page (right-aligned in the navbar; runs the full turn-resolution
and fog-of-war-inference speed sweep, streamed live over Server-Sent Events
from `GET /api/benchmark` — see `poke_rust::benchmarking`. This is the same
unbounded sweep the offline `cargo bench` binaries run, recorded in
`poke_rust/benches/RESULTS.md`, so it can take several minutes; the page
shows real backend-reported progress, not a fake timer).
React + Vite + TypeScript + Tailwind CSS v4.

## Running

Two processes, both from the repo root:

```sh
# 1. API server (release build matters — turn resolution is compute-heavy)
cd poke_rust
cargo run --release --bin server          # binds http://127.0.0.1:3001

# 2. Frontend dev server
cd frontend
npm install
npm run dev                               # http://localhost:5173, proxies /api → :3001
```

The server takes the same dex-path flags as the CLI (`--poke-dex`,
`--move-dex`, defaults point at `../pokemon_info/` so run it from
`poke_rust/`), plus `--port` (default 3001).

## Testing

`e2e/` has a Playwright suite (currently just tracker mode's input bar).
`playwright.config.ts`'s `webServer` array launches both halves of the stack
above automatically — `cargo build --release --bin server` once first so the
binary exists, then:

```sh
cd frontend
npx playwright install chromium   # once
npx playwright test
```

`reuseExistingServer` (the default outside CI) means it's fine to already have
both dev processes running locally; the suite reuses them instead of
relaunching. There's no unit-test runner configured yet — TypeScript/lint
checks are `npx tsc -b` and `npm run lint`.

## Architecture

```
src/
  api/types.ts        1:1 TS mirrors of the server DTOs (source of truth:
                      poke_rust/src/bin/server/dto.rs — keep in sync by hand)
  api/client.ts       typed fetch wrappers
  store/battleStore.ts  hotseat command wizard (zustand): P1 picks per-slot
                      commands, then P2, then both ship in one POST /turn
  store/settingsStore.ts  theme (light/dark/custom), persisted
  lib/storage.ts      localStorage schemas: pokerust.teams.v1, pokerust.formats.v1
  lib/sprites.ts      Showdown name → PokeAPI slug + sprite URL cache
                      (sprites are fetched at runtime, never committed)
  lib/eventText.ts    EventNode tree → log lines (chronological walk with a
                      slot→species resolver, since events carry slots not names)
  lib/trackerGrammar.ts  word-by-word autocomplete mirror of tracker_parse.rs's
                      keyword tables, for TrackerInputBar — completion-only,
                      never the source of truth (the server still validates)
  pages/simulate/     SetupPanel, BattleScreen, Arena, ControlPanel,
                      PokemonHUD, FieldIndicators, BattleLogSidebar,
                      TeamInfoSidebar
  pages/tracker/      TrackerSetupPanel, TrackerScreen, TrackerArena,
                      TrackerInputBar (the autocomplete editor — see "The
                      input bar" above), TrackerLogSidebar, TrackerTeamSidebar
  pages/benchmark/    BenchmarkChart, ProgressBar — hand-rolled inline-SVG bar
                      chart + determinate progress bar (no charting
                      dependency); used by pages/BenchmarkingPage.tsx
  store/trackerStore.ts  single-perspective session store for tracker mode —
                      no hotseat flip, no command wizard; owns the authored
                      script (committedTurns) and the live per-event preview
e2e/                  Playwright suite (tracker-input.spec.ts) — see "Testing"
```

## Tracker mode

Follows a real battle you're playing or watching elsewhere: instead of a move
selector, you type what happened (`o1 switch garchomp`, `p1 thunderbolt o1
45%`, `endofturn`, …) into a plain textarea and the server translates it into
the same `InformationEvent`/fog-of-war machinery the Simulate page's inference
engine already runs on. There is no simulated opponent and no per-slot command
flow — `POST /api/tracker/{id}/events` is the only turn-advancing call, and it
expects one or more complete turns (each ending in an `endofturn` line) in a
single request.

Because there's no opponent to simulate, `BattleView.p1`/`p2` for a tracker
session are rendered straight from the fog-of-war belief on the Rust side (see
`poke_rust/src/bin/server/tracker.rs`'s module doc) — the client-side handling
is unchanged from battle mode: `TrackerScreen` reuses `PokemonHUD` and
`FieldIndicators` as-is, and `TrackerLogSidebar` reuses `lib/eventText.ts`'s
`renderLog` unchanged, since both are pure functions of `BattleView`/
`TurnLogEntry[]` with no assumption baked in about where that data came from.

### The input bar

`TrackerInputBar.tsx` is a floating, single-line, Minecraft-chat-style
autocomplete editor — not a plain textarea. `lib/trackerGrammar.ts` mirrors
`tracker_parse.rs`'s fixed keyword tables (verbs, statuses, cant-reasons,
volatiles, weather/terrain, stat/effect tokens) to rank word-by-word
suggestions client-side, with a Levenshtein fallback when nothing prefix-
matches; species/move/ability suggestions come from `GET
/api/tracker/{id}/completions`, scoped to the Pokémon actually in the match
(both rosters' learnsets/ability pools — never the full dex). Item
suggestions reuse the existing `lib/items.ts` catalog directly (items aren't
species-constrained, so no round trip is needed).

Two commit tiers, matching the inference engine's own turn-atomic design (see
`poke_rust/src/information/README.md`):

- **Per event** (`Enter`) — `POST /api/tracker/{id}/preview` runs a
  Pass-1-only structural pass (`apply_structural_preview` on the Rust side) on
  a disposable clone of the committed belief, so obvious facts (HP, revealed
  species/moves, status/volatiles/boosts) render immediately as the user
  types. It never mutates the session — Pass 2 onward (item/stat inference,
  speed ordering, EV/IV back-solve, BCP) all reason about *absence* across a
  whole turn, which is unsound on a still-in-progress one.
- **Per turn / on edit** (`Shift+Enter`, or editing an already-committed
  event) — the frontend owns the full authored script as `committedTurns` in
  `trackerStore.ts` and resubmits it in full to `PUT
  /api/tracker/{id}/history`, which resets to the session's initial
  (pre-first-turn) belief and re-applies every turn through the real six-pass
  pipeline. This one endpoint uniformly handles ending a turn, correcting a
  past event (`ArrowUp` navigates the flat history — every line ever typed,
  committed or still-drafted — for in-place editing), and popping the current
  draft back via `Shift+Escape`, since none of those are actually different
  operations from the belief's point of view.

`tracker_parse.rs`'s module doc lists the current grammar's scope and known
simplifications (e.g. every targeted move needs an explicit target slot;
guaranteed effects cover a starter set of abilities/moves, not the full dex
yet). `e2e/tracker-input.spec.ts` (Playwright) drives the bar end-to-end
against the real server — autocomplete ranking/ghost-text, the two-tier
commit model, editing a past turn and watching the belief recompute, and the
Escape/Shift+Escape navigation contract.

### Tracker text grammar

Tracker input is line-oriented. A submission contains one or more complete
turns, and every turn ends with `endofturn` (or `eot`). Blank lines and lines
whose first non-whitespace character is `#` are ignored. Errors report the
1-based input line; the server applies the whole submission to a scratch belief
and commits nothing if any line or turn fails.

Names and keywords are case-insensitive and ignore punctuation. For example,
`SwordsDance`, `swords-dance`, and `swords_dance` are equivalent. Whitespace
still separates tokens, so multiword names must be joined (`rockyhelmet`, not
`rocky helmet`; `mr-mime`, not `mr mime`). A complete normalized dex name works
for any species, move, ability, or item; the aliases below are conveniences.

```text
submission  := turn+
turn        := (event-line | blank-line | comment-line)* ("endofturn" | "eot")
slot        := ("p" | "o") [positive-integer]
hpspec      := <unsigned-integer>("%" | "hp")
boostspec   := <stat><signed-integer> | <signed-integer><stat>
```

`p` is the tracker owner's side and `o` is the opponent. Slot numbers are
1-based from left to right; an omitted number means slot 1. Thus `p`, `p1`,
`o`, and `o1` are valid, while doubles also uses `p2` and `o2`.

Every occupied active slot on both sides must have an action in each ordinary
turn: a move, switch, cant reason, `mustrecharge`, or explicit `pass`. A slot
does not need another action if it was knocked out before acting or the battle
ended. A replacement-only turn requires switches only for slots whose fainted
occupants have healthy reserves; an already fainted slot with no reserve can
also be omitted.

#### Event lines

| Form | Meaning |
|---|---|
| `leads [p\|o] <species>... [p\|o] <species>...` | Send out one or both sides, left to right, on a single line: `leads p tyranitar raichu o charizard aerodactyl`. Any digit on `p1`/`o1` is ignored — a side marker always means "this whole side". Opening ability reveals should immediately follow the leads line. |
| `[slot] switch <species> [hpspec]` | Switch one slot. `switchin` and `sendout` are aliases. HP defaults to the known value for your roster and 100% for a newly seen opponent. |
| `[slot] mega [species-or-suffix]` | Mega Evolve. `megaevolve` and `megaevolution` are aliases. The form may be omitted only if the active species is known and has exactly one Mega; a suffix such as `y` disambiguates Charizard. |
| `[slot] tera <type>` | Terastallize into a type. `terastallize` and `terastallized` are aliases. |
| `[slot] <move> [target-or-effect]...` | Record a move and its observed results; see “Move lines” below. |
| `[slot] <ability>` | Reveal an ability, such as `o1 intimidate`. |
| `[slot] <item>` | Reveal a held item without losing it, such as `o1 leftovers`. |
| `[slot] <item-verb> <item>` | Record an item being lost, consumed, or gained; see the item verbs below. |
| `[slot] hp <hpspec>` | Record HP outside a move line, usually residual damage or healing. |
| `[slot] <cant-reason>` | Record why the slot could not act. This form must contain exactly two tokens. |
| `[slot] mustrecharge` | Record the move-created must-recharge state. This is distinct from the `recharge` cant reason on the following turn. |
| `[slot] charging <move>` | Record the charge turn of a two-turn move. Equivalent to the move line `[slot] <move> charging`, which is the preferred form; see “Charge turns” below. |
| `[slot] pass` | Explicitly record no action, including a skipped action after the battle has already ended. |
| `weather <weather>` | Set or clear the weather. |
| `terrain <terrain>` | Set or clear the terrain. |

A single `leads` line covering both sides parses directly to one simultaneous
switch event; a turn that instead spells the two sides out as separate
consecutive `leads p ...` / `leads o ...` lines is merged into the same shape.
Ability lines for those entrants that immediately follow are nested into that
event, preserving entry-ability and ability-absence inference.

#### Move lines

A move line starts with its user and move. Every targeted move must name each
target explicitly, even in singles — the one exception being a charge turn,
which may name none (see “Charge turns” below):

```text
p1 thunderbolt o1 45% par
p1 rockslide o1 62% o2 miss
o1 tackle p1 88hp p1 helmet o1 91%
```

Reading left to right, a slot token changes the current attachment point. Each
following effect belongs to the most recently named slot until another slot is
seen. Naming a non-user slot also adds it to the move's target list. Repeating a
slot is allowed, which is how multi-hit HP readings are represented.

| Effect token | Accepted forms |
|---|---|
| Critical hit | `crit` |
| Miss | `miss`, `missed` |
| Immunity | `immune` |
| Protection/block | `block`, `blocked` |
| Whole-move failure | `fail`, `failed` |
| Charge turn of a two-turn move | `charging` |
| HP after the effect | `45%` for masked percent HP, or `97hp` for exact HP |
| Stat change | `atk+1`, `+1atk`, `spe-2`, `-2spe`, and the stat aliases below |
| Major status inflicted | Any status word below |
| Volatile started | Any volatile word below |
| Ability revealed | Any complete normalized ability name |
| Item revealed | Any complete normalized item name or item alias |
| Item changed | An item verb followed by an item, e.g. `ate sitrus` |

An HP token is classified by comparing it with that slot's believed HP at the
start of the submitted batch: lower is damage, higher is healing, and unchanged
is a direct HP set. If the old and new representations differ (exact HP versus
percent) or no old reading exists, it is treated as damage. Own-side readings
should normally use exact `hp`; opponent readings should normally use `%`. If
HP direction in a later turn depends on an earlier turn's new value, submit the
turns separately so the second parse sees the updated belief.

`miss`, `immune`, or `blocked` suppresses guaranteed target effects for that
target. `fail` suppresses guaranteed effects for the entire move. Chance-based
effects must be typed when observed: for example, include `par` when
Thunderbolt actually paralyzes, but omit it otherwise.

#### Charge turns

The first turn of a two-turn move is a normal move line with a `charging`
token, and — unlike every other targeted move — it may name **no target at
all**, because the charge step usually does not reveal one:

```text
o1 solarbeam charging
p1 protect
endofturn

o1 solarbeam p2 45% crit
p2 substitute
endofturn
```

Give a target if you do know it. `charging` takes no argument: the move being
charged is always the line's own move, so repeating it (`o1 solarbeam charging
solarbeam`) is accepted but pointless, and naming a different move is an error.
`o1 charging solarbeam` is also accepted and means the same thing.

`charging` suppresses the move's own effects for that turn — they belong to the
release turn — and adds the charge-turn boost for the moves that have one
(Meteor Beam and Electro Shot raise Special Attack, Skull Bash raises Defense).
Geomancy is not one of those: its boosts land when it fires.

Because the suppression keys on the token rather than on the move, a use that
skips the charge entirely needs no special handling — a Power Herb Geomancy,
Solar Beam in harsh sun, or Electro Shot in rain is just an ordinary one-turn
move line with no `charging` token. The one gap: for Meteor Beam, Electro Shot,
and Skull Bash, a Power Herb one-turn use is indistinguishable from a release
turn, so type the charge-turn boost yourself (`p1 meteorbeam o1 spa+1`).

A slot that spent its turn charging counts as having acted, and a move aimed at
a slot that is mid-Fly, Dig, Dive, Bounce, Phantom Force, Shadow Force, or Sky
Drop has its guaranteed effects suppressed automatically — you do not need to
type `miss`.

#### Accepted words and aliases

- Stats: `atk`/`attack`; `def`/`defense`/`defence`;
  `spa`/`spatk`/`spattack`/`specialattack`;
  `spd`/`spdef`/`spdefense`/`specialdefense`; `spe`/`speed`;
  `acc`/`accuracy`; `eva`/`evasion`/`evasiveness`.

- Major statuses: burn (`brn`, `burn`, `burned`); poison (`psn`, `poison`,
  `poisoned`); toxic poison (`tox`, `badpoison`, `badlypoisoned`, `toxic`);
  paralysis (`par`, `para`, `paralyzed`, `paralysis`, `paralysed`); sleep
  (`slp`, `sleep`, `asleep`); freeze (`frz`, `frozen`, `freeze`).

- Weather: rain (`rain`, `raindance`, `drizzle`); heavy rain (`heavyrain`,
  `primordialsea`); sandstorm (`sand`, `sandstorm`); snow (`snow`, `hail`);
  sun (`sun`, `sunnyday`, `sunny`, `drought`); extreme sun (`extremesun`,
  `desolateland`, `harshsunlight`); strong winds (`strongwinds`,
  `deltastream`). Use `none` or `clear` to remove weather.

- Terrain: Electric, Grassy, Misty, and Psychic accept either the short name
  (`electric`) or full name (`electricterrain`). Use `none` or `clear` to
  remove terrain.

- Item verbs: lost (`loses`, `lost`, `knockedoff`); consumed (`consumes`,
  `consumed`, `ate`, `eats`, `used`); gained (`gains`, `gained`, `tricked`,
  `switcheroo`, `recycles`).

- Item aliases: `sitrus`, `lum`, `chesto`, `lefties`/`levs`, `helmet`, `lo`,
  `scarf`, `specs`, `band`, `boots`, `wp`, `av`, and `sash` expand to Sitrus
  Berry, Lum Berry, Chesto Berry, Leftovers, Rocky Helmet, Life Orb, the three
  Choice items, Heavy-Duty Boots, Weakness Policy, Assault Vest, and Focus
  Sash respectively.

- Cant reasons: `flinch`/`flinched`; `fullpara`/`fullyparalyzed`/
  `fullparalysis`/`fullyparalysed`; `sleep`/`asleep`/`slp`;
  `frozen`/`frz`/`freeze`; `recharge`/`mustrecharge`/`recharging`;
  `taunt`/`taunted`; `disable`/`disabled`; `confusion`/`confused`;
  `imprison`/`imprisoned`; `attract`/`infatuated`/`infatuation`;
  `bound`/`trapped`; `throatchop`/`throatchopped`; `torment`/`tormented`;
  `focuspunch`; `gravity`; `healblock`; `encore`/`encored`; `skydrop`; and
  `beakblast`.

- Volatiles: `confusion`/`confused`, `leechseed`/`seeded`,
  `taunt`/`taunted`, `flashfire`, `focusenergy`, `aquaring`,
  `attract`/`infatuated`, `curse`/`cursed`, `torment`/`tormented`, `yawn`,
  `saltcure`, `tarshot`, `minimize`/`minimized`, `ingrain`, `magnetrise`,
  `protect`/`protected`, `endure`/`enduring`, `kingsshield`, `banefulbunker`,
  `spikyshield`, `silktrap`, `obstruct`, `burningbulwark`, `destinybond`,
  `grudge`, `embargo`, `healblock`, `imprison`, `electrify`, `powder`,
  `syrupbomb`, `telekinesis`, `smackdown`, `uproar`, `roost`, `rage`,
  `ragepowder`, `followme`, `magiccoat`, `snatch`, `laserfocus`,
  `miracleeye`, `foresight`, `octolock`, `noretreat`, `gastroacid`,
  `sparklingaria`, `glaiverush`, `charge`/`charged`,
  `defensecurl`/`defensecurled`, `helpinghand`, `powertrick`, and
  `forestscurse`.

#### Effects filled in automatically

Users type observations, while the server adds consequences guaranteed by
those observations:

- A revealed supported entry ability adds its deterministic reaction:
  Intimidate, weather/terrain setters, Intrepid Sword, Dauntless Shield,
  unambiguous Download, and unambiguous Trace. The ordinary ability reveal
  itself must still be typed. Mega Evolution automatically reveals its Mega
  form's fixed ability and applies the same reactions.
- A move adds its always-on self boost and all structured 100%-chance move-dex
  effects, including guaranteed boosts, status, weather, terrain, and side
  conditions. Random secondaries are never guessed.
- Any recorded HP value of zero adds the corresponding faint event.

Recoil, drain, reactive item/ability effects, and other observed consequences
that are not guaranteed by the structured move data must be written explicitly.

#### Phase-1 limits

The grammar currently represents starts of payload-free volatiles only. It
cannot directly express payload-bearing volatiles such as Disable/Locked Move/
Choice Lock with their move, or Substitute with its HP. Slot-condition payloads
such as Wish, Future Sight, and Doom Desire are not synthesized. There are also
no general-purpose forms yet for curing statuses, ending volatiles or side
conditions, form changes, Transform, or Illusion ending. Unrecognized input is
rejected rather than guessed.

Tracker fuzz coverage lives in the server binary tests. The default sweep runs
deterministic full battles through simulator events -> tracker text -> parser ->
the production submission pipeline. Seeds are replayable and scalable:

```sh
cd poke_rust
cargo test --bin server randomized_tracker_text_round_trips_do_not_contradict
POKERUST_TRACKER_FUZZ_ITERS=1000 cargo test --release --bin server randomized_tracker_text_round_trips_do_not_contradict -- --nocapture
POKERUST_TRACKER_FUZZ_SEED_START=42 POKERUST_TRACKER_FUZZ_ITERS=1 POKERUST_TRACKER_FUZZ_REPLAY=1 cargo test --release --bin server randomized_tracker_text_round_trips_do_not_contradict -- --nocapture
```

The stronger truth-subset oracle is opt-in, matching the inference fuzz suite
while its shared inference-engine over-narrowing families remain open:
`cargo test --release --bin server -- --ignored randomized_tracker_text_beliefs_stay_sound_subset`.

## Notes

- **Hotseat model**: the server never holds half a turn. The frontend collects
  P1's full command set, flips to P2, then submits both together.
- **Doubles targeting**: legal targets come from the server's pre-expanded
  command options — the client has no targeting rules. A multi-target move
  parks in `pendingAttack` and the Arena highlights clickable target slots.
- **Damage rolls**: the server resolves turns with the engine's sample mode
  (`simulator::sample_turn`) — one weighted trajectory instead of the full
  outcome tree — so every format runs at full 16-roll granularity. The
  `probability` in the turn response is the joint probability of the sampled
  trajectory.
- **Battle restore**: the active battle id lives in sessionStorage; a page
  refresh re-fetches state + full event log from `GET /api/battles/{id}`.
  Server sessions are in-memory — restarting the server loses battles.
- **Sprites**: resolved through the PokeAPI `pokemon/{slug}` endpoint with an
  exception table for forme names, cached in localStorage. Unknown slugs fall
  back to the species endpoint's default variety, then progressively strip
  forme suffixes (Champions-only megas render the base species sprite); total
  failures show a gray Pokéball placeholder.
- **Item catalog**: `lib/items.ts` is a static list — exactly the Pokémon
  Champions held-item pool (general items, Mega Stones, berries) — not a
  PokeAPI fetch. Item sprite slugs still resolve against the PokeAPI sprites
  repo; Champions-only Mega Stones have no sprite and render label-only.
