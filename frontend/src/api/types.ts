// 1:1 mirrors of the server DTOs in poke_rust/src/bin/server/dto.rs.
// Keep these in sync by hand — the Rust side is the source of truth.

export type PlayerId = 'p1' | 'p2'

export interface FieldSlot {
  player: PlayerId
  slotIndex: number
}

/** HP as observed by a real player: exact for their own side, percent for the
 * opponent's. Used both for the event stream and for `PokemonView.hp`. */
export interface ObservedHp {
  exact?: number
  percent?: number
}

export interface Status {
  /** Badge code: BRN, PSN, TOX, PAR, SLP, FRZ. */
  code: string
  turns?: number
}

export interface Volatile {
  name: string
  turns?: number
}

/** A field/side effect's name plus how long it has left. Under fog-of-war the
 * exact remaining count is frequently not knowable (e.g. weather's base 5
 * turns vs. 8 with an extension rock the setter's item hasn't revealed) —
 * `turns` is always the lower bound (or the exact value once collapsed) and
 * `turnsMax` is the upper bound, present ONLY when it differs from `turns`. */
export interface NamedTurns {
  name: string
  turns?: number
  turnsMax?: number
}

export interface MoveView {
  name: string
  pp: number
  maxPp: number
}

export interface PokemonView {
  monId: number
  /** Showdown display name, e.g. "Abomasnow-Mega" — also the sprite-slug source.
   * Always the physically-displayed appearance (the Illusion disguise when one is
   * active) regardless of information mode — a real player always sees this, it's
   * never secret team-sheet info. */
  species: string
  level: number
  gender: string
  types: string[]
  hp: ObservedHp
  fainted: boolean
  status: Status | null
  volatiles: Volatile[]
  /** HP, Atk, Def, SpA, SpD, Spe. Under a masked (non-Perfect-Information) view this
   * is the LOWER bound of the stat range; equal to `statsMax` for ground truth. */
  stats: [number, number, number, number, number, number]
  /** Upper bound of the stat range — equal to `stats` unless masked and the range
   * hasn't collapsed to a point yet. */
  statsMax: [number, number, number, number, number, number]
  /** Atk, Def, SpA, SpD, Spe, Acc, Eva stages */
  boosts: [number, number, number, number, number, number, number]
  nature: string
  /** HP, Atk, Def, SpA, SpD, Spe EVs — lower bound under a masked view, see `stats`. */
  evs: [number, number, number, number, number, number]
  /** Upper bound of the EV range, see `statsMax`. */
  evsMax: [number, number, number, number, number, number]
  item: string | null
  ability: string
  moves: (MoveView | null)[]
  isTera: boolean
  teraType: string
  isMega: boolean
  /** `true` when this mon's species is still an unresolved multi-candidate Illusion
   * disguise in the observer's belief (always `false` for ground truth). */
  isIllusionSuspected: boolean
}

export interface SideView {
  active: PokemonView[]
  back: PokemonView[]
  /** Species shown at team preview but not brought into this battle (a bring-N-of-M
   * format gap) — rendered grayed-out. Always empty for P1 and under Perfect
   * Information. */
  possibleBack?: PokemonView[]
  /** Opponent mons that fainted and were then replaced — the fog belief keeps their
   * accumulated knowledge (species, revealed moves/item/ability) here instead of
   * discarding it. Always empty for P1 and under Perfect Information, where a
   * fainted mon's knowledge already rides in `active`/`back` with `fainted: true`. */
  fainted?: PokemonView[]
  canTera: boolean
  canMega: boolean
  sideConditions: NamedTurns[]
  slotConditions: string[][]
}

export interface FieldView {
  weather: NamedTurns | null
  terrain: NamedTurns | null
  pseudoWeathers: NamedTurns[]
}

export interface PreviewView {
  activePerSide: number
  broughtPerSide: number
  p1Mons: PokemonView[]
  p2Mons: PokemonView[]
}

export type Phase = 'teamPreview' | 'normal' | 'selfSwitch' | 'replacement' | 'gameOver'

/** The engine's CNF predicate store rendered as plain-English OR-clauses — literally
 * "a list of ORs". Absent under Perfect Information (no belief tracked) or during
 * team preview (no predicates yet); the Predicates tab only appears when present. */
export interface BeliefView {
  clauses: string[]
}

export interface BattleView {
  phase: Phase
  turnNumber: number
  activePerSide: number
  broughtPerSide: number
  preview?: PreviewView
  p1?: SideView
  p2?: SideView
  field?: FieldView
  selfSwitch?: FieldSlot
  winner?: PlayerId
  belief?: BeliefView
}

// ── Commands ────────────────────────────────────────────────────────────────

export type BattleCommand =
  | {
      kind: 'attack'
      moveSlot: number
      target?: FieldSlot
      terastallize?: boolean
      megaEvolve?: boolean
    }
  | { kind: 'switch'; partyIndex: number }
  | { kind: 'struggle'; target?: FieldSlot }
  | { kind: 'pass' }

export type PlayerCommand =
  | { kind: 'battle'; commands: BattleCommand[] }
  | { kind: 'pass' }
  | { kind: 'teamPreview'; activeIndices: number[]; backIndices: number[] }

export interface CommandOption {
  command: BattleCommand
  description: string
  label?: string
}

export interface SlotCommands {
  slotIndex: number
  forced: boolean
  options: CommandOption[]
}

export interface LegalCommands {
  phase: Phase
  slots: SlotCommands[]
}

// ── Events ──────────────────────────────────────────────────────────────────

export interface SwitchInfo {
  slot: FieldSlot
  species: string
  level: number
  hp: ObservedHp
  status?: Status
  teraType?: string
}

/** Tagged by `type`; field names mirror EventKindDto in dto.rs. */
export type EventKind =
  | { type: 'moveUsed'; user: FieldSlot; move: string; targets: FieldSlot[] }
  | { type: 'switch'; switch: SwitchInfo }
  | { type: 'simultaneousSwitch'; switches: SwitchInfo[] }
  | { type: 'endOfTurn' }
  | { type: 'faint'; slot: FieldSlot }
  | { type: 'megaEvolution'; slot: FieldSlot; into: string }
  | { type: 'terastallization'; slot: FieldSlot; teraType: string }
  | { type: 'formeChange'; slot: FieldSlot; into: string; permanent: boolean }
  | { type: 'typeChanged'; slot: FieldSlot; newTypes: string[] }
  | { type: 'cant'; slot: FieldSlot; reason: string }
  | { type: 'chargingMove'; user: FieldSlot; move: string }
  | { type: 'mustRecharge'; slot: FieldSlot }
  | { type: 'singleMoveOrTurn'; slot: FieldSlot; move: string }
  | { type: 'damageDealt'; target: FieldSlot; newHp: ObservedHp }
  | { type: 'healed'; target: FieldSlot; newHp: ObservedHp }
  | { type: 'setHp'; target: FieldSlot; newHp: ObservedHp }
  | { type: 'crit'; target: FieldSlot }
  | { type: 'immune'; target: FieldSlot }
  | { type: 'missed'; target: FieldSlot }
  | { type: 'moveFailed'; slot: FieldSlot }
  | { type: 'blocked'; target: FieldSlot }
  | { type: 'hitCount'; target: FieldSlot; hits: number }
  | { type: 'statusInflicted'; target: FieldSlot; status: Status }
  | { type: 'statusCured'; target: FieldSlot; status: Status }
  | { type: 'teamStatusCured'; side: PlayerId }
  | { type: 'boostChanged'; target: FieldSlot; stat: string; stages: number }
  | { type: 'boostsCleared'; target: FieldSlot }
  | { type: 'boostsInverted'; target: FieldSlot }
  | { type: 'boostsSwapped'; source: FieldSlot; target: FieldSlot }
  | { type: 'boostsCopied'; source: FieldSlot; target: FieldSlot }
  | { type: 'weatherChanged'; weather: string | null }
  | { type: 'terrainChanged'; terrain: string | null }
  | { type: 'pseudoWeatherStart'; effect: string }
  | { type: 'pseudoWeatherEnd'; effect: string }
  | { type: 'sideConditionStart'; side: PlayerId; condition: string }
  | { type: 'sideConditionEnd'; side: PlayerId; condition: string }
  | { type: 'slotConditionStart'; slot: FieldSlot; condition: string }
  | { type: 'slotConditionEnd'; slot: FieldSlot; condition: string }
  | { type: 'volatileStart'; target: FieldSlot; volatile: string }
  | { type: 'volatileEnd'; target: FieldSlot; volatile: string }
  | { type: 'perishCount'; target: FieldSlot; turnsLeft: number }
  | { type: 'itemRevealed'; slot: FieldSlot; item: string }
  | { type: 'itemGained'; slot: FieldSlot; item: string }
  | { type: 'itemLost'; slot: FieldSlot; item: string; consumed: boolean }
  | { type: 'abilityRevealed'; slot: FieldSlot; ability: string }
  | { type: 'anticipationShudder'; slot: FieldSlot }
  | { type: 'illusionEnded'; slot: FieldSlot; actualSpecies: string }
  | { type: 'transformed'; slot: FieldSlot; intoSlot: FieldSlot; intoSpecies: string }

export type EventNode = EventKind & { reactions: EventNode[] }

export interface TurnLogEntry {
  label: string
  events: EventNode[]
}

// ── Requests / responses ────────────────────────────────────────────────────

/** `'closedSheet'` (default) | `'perfect'` | `'openSheet'` | `'openSheetNatures'`.
 * Selects the fog-of-war starting baseline for P1's view of P2 — see
 * `InformationMode` in the Rust engine. `closedSheet` is the traditional
 * VGC/Champions competitive format: only the opponent's species are visible at
 * team preview. */
export type InformationMode = 'closedSheet' | 'perfect' | 'openSheet' | 'openSheetNatures'

/** `'sheet'` (default): use the matching `p1Team`/`p2Team` text as pasted.
 * `'meta'`: ignore that text and have the server generate a fresh team from
 * Champions usage stats instead (the Meta Team Generator). Mirrors
 * `CreateBattleRequest.p1_team_mode`/`p2_team_mode` in `dto.rs`. */
export type TeamMode = 'sheet' | 'meta'

export interface CreateBattleRequest {
  /** Ignored when the matching `TeamMode` is `'meta'` — send `''`. */
  p1Team: string
  p2Team: string
  p1TeamMode?: TeamMode
  p2TeamMode?: TeamMode
  /** Seeds the Meta Team Generator's draw for reproducibility. Omitted picks a
   * fresh random seed per request. Unused when both `TeamMode`s are `'sheet'`. */
  metaSeed?: number
  activePerSide: number
  broughtPerSide: number
  statPoints?: boolean
  considerCrit?: boolean
  /** Pin all opponent IVs to 31 for the fog-of-war inference engine (Champions
   * competitive default). Mirrors `InferenceConfig::force_max_ivs` in the Rust engine. */
  forceMaxIvs?: boolean
  damageRolls?: number
  informationMode?: InformationMode
  /** The selected format's full item catalog minus its banned items (slugs from
   * `lib/items.ts`'s `CATALOG`, filtered by `StoredFormat.bannedItems`). Empty/
   * omitted means no restriction. Mirrors `CreateBattleRequest::legal_items` in
   * `dto.rs` — an unrecognized slug is rejected with 422. */
  legalItems?: string[]
}

export interface CreateBattleResponse {
  battleId: string
  /** P1's fog-of-war view of the battle. */
  state: BattleView
  /** P2's fog-of-war view of the same battle — mirrors `state` with the masked side
   * flipped. Pick between the two based on whose perspective is currently shown
   * (see `battleStore.ts`'s `currentPlayer`). */
  stateP2: BattleView
}

export interface GetBattleResponse {
  state: BattleView
  stateP2: BattleView
  /** P1's turn log — every turn's events masked for P1's perspective. */
  log: TurnLogEntry[]
  /** P2's turn log — the same turns, masked for P2's perspective instead. */
  logP2: TurnLogEntry[]
}

export interface TurnRequest {
  p1: PlayerCommand
  p2: PlayerCommand
}

export interface TurnResponse {
  state: BattleView
  stateP2: BattleView
  /** This turn's events masked for P1's perspective. */
  events: EventNode[]
  /** This turn's events masked for P2's perspective instead. */
  eventsP2: EventNode[]
  probability: number
}

// ── Benchmarking ────────────────────────────────────────────────────────────
// Mirrors `poke_rust::benchmarking`'s result rows via `src/bin/server/dto.rs`'s
// `TurnSpeedRowDto`/`InferenceRowDto`/`SolverRowDto`/`BenchmarkResultDto`/
// `BenchmarkProgressDto`. `GET /api/benchmark` streams over Server-Sent Events
// — no request body or knobs, always the full unbounded grid.
//
// The three sweeps run concurrently server-side and each reports on its own
// schedule: tagged `progress` events throughout, then one `result` per sweep the
// moment it finishes, a `failed` that takes down only its own sweep, and a
// single terminal `done`. That is what lets each chart render as it lands.
//
// Because the sweeps share the machine, the times are contended and do NOT
// reproduce `poke_rust/benches/RESULTS.md`'s serial numbers — relative shape
// within a sweep holds, absolute microseconds do not. Count columns (branches,
// turns simulated, cells) are unaffected. `BenchmarkingPage` says so on screen.

export interface TurnSpeedRow {
  scenario: 'singles' | 'doubles'
  mode: 'enumerate' | 'sample'
  rolls: number
  crit: boolean
  avgTimeSecs: number
  avgBranches: number
  pairings: number
}

export interface InferenceRow {
  scenario: 'singles' | 'doubles'
  informationMode: string
  calls: number
  avgTimeSecs: number
  contradictions: number
  /** A real caught panic message from the first contradiction — always an
   * `apply_information` panic (typically the `inference_contradiction!`
   * macro; see known families in `TODO.md`'s "Fixes" section), never a
   * `[subset violation]` (a separate, unrelated check this sweep never
   * runs). Absent when `contradictions === 0`. */
  contradictionSample?: string
}

/** One `(scenario, algorithm, depth, rolls, chance)` cell of the game-tree
 * solver sweep.
 *
 * `avgTurnsSimulated` is the cost number that matters, not `avgTimeSecs`: a
 * `simulate_turn` call outweighs a matrix LP by three orders of magnitude, and
 * unlike wall-clock it is unaffected by the sweeps sharing the machine.
 * `avgCellsEvaluated` against `avgCellsTotal` is what the pruning bought. */
export interface SolverRow {
  scenario: 'singles' | 'doubles'
  algorithm: 'backwardInduction' | 'serializedBounds' | 'doubleOracle'
  depth: number
  rolls: number
  chance: string
  /** Joint-action cap in force. Absent means the full action set was used. */
  actionCap?: number
  avgTimeSecs: number
  avgNodes: number
  avgTurnsSimulated: number
  avgCellsEvaluated: number
  avgCellsTotal: number
  avgLps: number
  pairings: number
  /** Why the cell was not attempted. Absent when it ran. */
  skipped?: string
}

export type BenchmarkSweep = 'turnSpeed' | 'inference' | 'solver'

/** One finished sweep. Tagged rather than an object of three optional arrays,
 * so "reported no rows" can never be mistaken for "has not finished yet". */
export type BenchmarkResult =
  | { sweep: 'turnSpeed'; rows: TurnSpeedRow[] }
  | { sweep: 'inference'; rows: InferenceRow[] }
  | { sweep: 'solver'; rows: SolverRow[] }

/** One `progress` SSE event — `stage` says which sweep it belongs to. All three
 * interleave, so progress must be tracked per sweep rather than globally. */
export interface BenchmarkProgress {
  stage: BenchmarkSweep
  completed: number
  total: number
}

/** One sweep failed. Not terminal for the stream — the others keep going. */
export interface BenchmarkSweepError {
  sweep: BenchmarkSweep
  message: string
}

// ── Tracker mode ────────────────────────────────────────────────────────────
// A second, simpler session kind for following a real battle by typing what
// happened instead of driving a simulated opponent — mirrors the tracker DTOs
// in `poke_rust/src/bin/server/dto.rs`. There's no `PlayerCommand`/turn-
// resolution flow here: the server translates submitted text into the same
// event vocabulary `BattleView`/`EventNode` above already use, so a tracker
// session renders through the exact same `BattleView` type and log/field
// components as battle mode.

export interface CreateTrackerRequest {
  /** The tracker viewer's own full roster, as a Showdown teamsheet. */
  myTeam: string
  /** The opponent's roster: a Showdown teamsheet, OR 6 comma-separated species
   * names (the server splits a single comma-separated line into per-mon
   * blocks). Only species matters — the server discards any item/ability/
   * moves this happens to specify. */
  opponent: string
  activePerSide: number
  broughtPerSide: number
  statPoints?: boolean
  forceMaxIvs?: boolean
  /** `'closedSheet'` (default) | `'openSheet'` | `'openSheetNatures'` — no
   * `'perfect'` option here, it has no meaning without a simulated opponent. */
  informationMode?: 'closedSheet' | 'openSheet' | 'openSheetNatures'
  legalItems?: string[]
}

export interface CreateTrackerResponse {
  trackerId: string
  state: BattleView
}

export interface GetTrackerResponse {
  state: BattleView
  log: TurnLogEntry[]
  /** The full raw tracker-text script committed so far (every turn,
   * newline-joined) — lets the editor rehydrate its authored text after a
   * page reload; empty string if nothing has been committed yet. */
  script: string
}

export interface TrackerEventsRequest {
  /** One or more complete turns of tracker-syntax text, each terminated by an
   * `endofturn` line. */
  text: string
}

/** Returned (422) when the text fails to parse — points at the offending line
 * rather than a generic message. */
export interface TrackerParseError {
  line: number
  message: string
}

export interface TrackerEventsResponse {
  state: BattleView
  /** The newly-committed turn(s) this submission produced — append to the
   * client's existing log rather than replacing it. */
  logDelta: TurnLogEntry[]
}

/** `POST /api/tracker/{id}/preview` request — the current in-progress turn's
 * tracker-syntax text so far. Unlike `TrackerEventsRequest`, this need not be
 * a complete turn and need not end with `endofturn`. */
export interface TrackerPreviewRequest {
  text: string
}

/** `POST /api/tracker/{id}/preview` response — a disposable, Pass-1-only
 * structural view (see `apply_structural_preview` on the Rust side): only
 * directly-confirmed facts (species/move reveals, HP, status, volatiles,
 * boosts), never the full six-pass inference — that needs a genuinely
 * complete turn. Never persisted; superseded the instant the turn is actually
 * committed via `/events` or `/history`. */
export interface TrackerPreviewResponse {
  state: BattleView
  /** The in-progress turn's events, parsed and guaranteed-effect-augmented
   * exactly like a committed `TurnLogEntry`'s events — renders through the
   * same `renderLog` path as a real log entry instead of raw text. */
  events: EventNode[]
}

/** `GET /api/tracker/{id}/completions` response — autocomplete name pools
 * scoped to the species actually in THIS match (both rosters, known from team
 * preview regardless of fog-of-war). `moves`/`abilities` are the union of
 * those species' learnsets/ability pools — never the full dex, since a move
 * no Pokemon in the match could ever have isn't a useful suggestion. Items
 * are deliberately absent (they aren't species-constrained); the frontend
 * already owns the full item catalog — see `lib/items.ts`'s `CATALOG`. */
export interface TrackerCompletionsDto {
  species: string[]
  moves: string[]
  abilities: string[]
}

/** `GET /api/dex/species` response — every teamsheet-legal species,
 * alphabetically, as display names.
 *
 * Session-free, unlike `TrackerCompletionsDto`: this backs the tracker SETUP
 * form's opponent picker, which runs before any session exists. It's also the
 * one place a full dex dump is correct — the user is naming arbitrary
 * opponents, not picking from a known roster. Battle-only formes (Mega, Primal,
 * Ash-Greninja, …) are filtered server-side, since they can never appear on a
 * sheet. */
export interface SpeciesListDto {
  species: string[]
}
