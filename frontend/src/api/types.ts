// Mirrors the server DTOs in `poke_rust/src/bin/server/dto.rs`.
// Update this file by hand after a Rust DTO change.

export type PlayerId = 'p1' | 'p2'

export interface FieldSlot {
  player: PlayerId
  slotIndex: number
}

/** Stores exact ally HP or an opponent HP percentage. */
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

/** Stores a field-effect name and its remaining duration.
 * `turns` is the minimum or exact duration.
 * `turnsMax` contains a different maximum duration when necessary. */
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
  /** Visible Showdown species name and sprite source.
   * An active Illusion shows its disguise name. */
  species: string
  level: number
  gender: string
  types: string[]
  hp: ObservedHp
  fainted: boolean
  status: Status | null
  volatiles: Volatile[]
  /** HP, Atk, Def, SpA, SpD, and Spe.
   * A masked view stores the lower bounds. */
  stats: [number, number, number, number, number, number]
  /** Upper stat bounds.
   * Equal to `stats` when each stat is exact. */
  statsMax: [number, number, number, number, number, number]
  /** Atk, Def, SpA, SpD, Spe, Acc, Eva stages */
  boosts: [number, number, number, number, number, number, number]
  nature: string
  /** Lower HP, Atk, Def, SpA, SpD, and Spe EV bounds. */
  evs: [number, number, number, number, number, number]
  /** Upper EV bounds. */
  evsMax: [number, number, number, number, number, number]
  item: string | null
  ability: string
  moves: (MoveView | null)[]
  isTera: boolean
  teraType: string
  isMega: boolean
  /** True while the observer has multiple Illusion species candidates. */
  isIllusionSuspected: boolean
}

export interface SideView {
  active: PokemonView[]
  back: PokemonView[]
  /** Preview species that did not enter the selected team.
   * The interface shows these species in gray. */
  possibleBack?: PokemonView[]
  /** Replaced fainted opponents and their revealed data.
   * Perfect information keeps these entries in the normal team lists. */
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

/** CNF predicates rendered as readable OR clauses.
 * Perfect information and team preview omit this value. */
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

/** Tagged event that mirrors `EventKindDto` in `dto.rs`. */
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

/** Selects the initial opponent information for P1.
 * A closed sheet shows only opponent species at preview. */
export type InformationMode = 'closedSheet' | 'perfect' | 'openSheet' | 'openSheetNatures'

/** Selects a pasted team or a team generated from usage data. */
export type TeamMode = 'sheet' | 'meta'

/** The search that a P2 bot profile configures.
 * The first three names solve the game exactly to the depth horizon.
 * The last three names sample, so they return an estimate. */
export type BotAlgorithm =
  | 'backwardInduction'
  | 'serializedBounds'
  | 'doubleOracle'
  | 'mcts'
  | 'ismcts'
  | 'mccfr'

/** A named group of solver limits. */
export type BotPreset = 'fast' | 'balanced' | 'strong'

/** The optional solver profile that plays P2.
 * Every field is optional, and the preset fills each absent field.
 * The server rejects a limit that the algorithm cannot read. */
export interface BotProfileRequest {
  /** Defaults to `doubleOracle`. */
  algorithm?: BotAlgorithm
  /** Defaults to `balanced`. */
  preset?: BotPreset
  timeMs?: number
  /** Exact algorithms only. */
  nodeBudget?: number
  depth?: number
  /** The search is serial, so the server accepts only 1. */
  workers?: number
  /** Sampling algorithms only. */
  iterations?: number
  /** Belief searches only. */
  particles?: number
  /** Makes a sampled search reproducible.
   * The maximum value is `Number.MAX_SAFE_INTEGER`. */
  seed?: number
  maxActionsPerPlayer?: number
}

/** The resolved profile that the server returns. */
export interface BotProfileView {
  algorithm: BotAlgorithm
  preset: BotPreset
  /** True when the algorithm itself is exact.
   * A limit can still make the result approximate. */
  exact: boolean
  timeMs: number | null
  nodeBudget: number | null
  depth: number
  workers: number
  iterations: number | null
  particles: number | null
  seed: number | null
  maxActionsPerPlayer: number | null
  /** Each reason that the result can differ from the exact answer. */
  approximations: string[]
  /** Each knob that the server changed away from the preset value. */
  adjustments: string[]
}

export interface CreateBattleRequest {
  /** Send an empty string when the matching team mode is `meta`. */
  p1Team: string
  p2Team: string
  p1TeamMode?: TeamMode
  p2TeamMode?: TeamMode
  /** Makes generated teams reproducible.
   * Omit it to select a new random seed. */
  metaSeed?: number
  activePerSide: number
  broughtPerSide: number
  /** The roster size of the format. A generated team gets this many Pokemon. */
  totalPerSide: number
  statPoints?: boolean
  considerCrit?: boolean
  /** Sets all inferred opponent IVs to 31. */
  forceMaxIvs?: boolean
  teraEnabled?: boolean
  megaEnabled?: boolean
  damageRolls?: number
  informationMode?: InformationMode
  /** Permitted item slugs after format bans.
   * Empty or absent means no item restriction. */
  legalItems?: string[]
  /** An optional profile for the P2 bot. */
  botP2?: BotProfileRequest
}

export interface CreateBattleResponse {
  battleId: string
  /** P1's fog-of-war view of the battle. */
  state: BattleView
  /** P2's masked view of the same battle. */
  stateP2: BattleView
  /** The resolved solver profile of P2, or null for a hotseat battle. */
  botP2: BotProfileView | null
}

export interface GetBattleResponse {
  state: BattleView
  stateP2: BattleView
  /** Turn events masked for P1. */
  log: TurnLogEntry[]
  /** Turn events masked for P2. */
  logP2: TurnLogEntry[]
  /** The resolved solver profile of P2, or null for a hotseat battle. */
  botP2: BotProfileView | null
}

export interface TurnRequest {
  p1: PlayerCommand
  /** The P2 command of a hotseat battle.
   *
   * A session with a P2 bot must omit this field, because the server draws
   * P2's command itself. A session with no bot must send it. Each broken rule
   * returns HTTP 422. */
  p2?: PlayerCommand
}

export interface TurnResponse {
  state: BattleView
  stateP2: BattleView
  /** This turn's events masked for P1. */
  events: EventNode[]
  /** This turn's events masked for P2. */
  eventsP2: EventNode[]
  probability: number
  /** The command that the server drew for P2, or absent for a hotseat battle. */
  p2Reveal?: P2Reveal
}

/** The drawn P2 command of one bot turn.
 *
 * The reveal carries one action and nothing else of P2's plan: no probability
 * of that action, no second action, and no win odds. The server returns it only
 * with the resolved turn, so both commands are already locked. */
export interface P2Reveal {
  /** The drawn command of each active slot, rendered against the position
   * before the turn.
   *
   * Empty at team preview. The leads appear on the field of their own accord,
   * and the back picks stay hidden under the fog of war. */
  commands: CommandOption[]
  /** Which rule produced the draw. */
  source: 'strategy' | 'uniform' | 'teamPreview'
  /** The seed of the draw. */
  drawSeed: number
  /** The replay record of the search that supplied the strategy.
   * Absent for either uniform draw. */
  replay?: AnalysisReplay
}

/** The data that repeats one analysis search. */
export interface AnalysisReplay {
  /** These values identify the position in the session history. */
  generation: number
  turnNumber: number
  /** The seed of the search. */
  searchSeed: number
  algorithm: string
  preset: string
  timeMs: number | null
  nodeBudget: number | null
  depth: number
  workers: number
  iterations: number | null
  particles: number | null
  maxActionsPerPlayer: number | null
  damageRolls: number
  considerCrit: boolean
}

/** The phase of the P2 analysis job. */
export type AnalysisPhase = 'idle' | 'running' | 'complete' | 'failed'

/** The cost of one complete analysis job.
 *
 * The row carries wall-clock cost alone. A node count or a turn-simulation
 * count divides by P1's own action count to give P2's, so the server sends
 * neither. */
export interface AnalysisCheckpoint {
  /** The generation of the position that the search read. */
  generation: number
  /** True when a later state change made this answer old. */
  stale: boolean
  turnNumber: number
  /** The depth that the search reached. */
  depthReached: number
  elapsedMs: number
  /** The seed of this search, which makes the result reproducible.
   * The server draws it below `Number.MAX_SAFE_INTEGER`, so it round-trips. */
  seed: number
  /** Every reason that the answer is approximate. */
  warnings: string[]
}

/** The private progress of the P2 analysis job.
 *
 * `GET /api/battles/{id}/analysis` returns progress alone. It carries no P2
 * action, no P2 strategy, and no P2 win odds, because P1 reads the same
 * endpoint during a hotseat battle. */
export interface AnalysisProgressResponse {
  /** The generation of the current position.
   * Every state change raises it by one. */
  generation: number
  phase: AnalysisPhase
  /** How long the running job has run, or null when no job runs. */
  runningMs: number | null
  /** The last complete answer.
   * A failure and a cancellation both keep it. */
  checkpoint: AnalysisCheckpoint | null
  /** Why the last job produced no checkpoint. */
  error: string | null
}

// ── Benchmarking ────────────────────────────────────────────────────────────
// Mirrors the benchmark DTOs in `src/bin/server/dto.rs`.
// `GET /api/benchmark` streams the complete grid through server-sent events.
// The server runs the three tests in sequence.
// Each test sends `progress`, then `result` or `failed`.
// One final `done` event ends the stream.

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
  /** First caught `apply_information` contradiction.
   * Absent when no contradiction occurs. */
  contradictionSample?: string
}

/** One solver benchmark configuration.
 * `avgTurnsSimulated` measures the main solver cost.
 * The cell counts show the work removed by pruning. */
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

/** Result from one completed benchmark test. */
export type BenchmarkResult =
  | { sweep: 'turnSpeed'; rows: TurnSpeedRow[] }
  | { sweep: 'inference'; rows: InferenceRow[] }
  | { sweep: 'solver'; rows: SolverRow[] }

/** Progress event for the test named by `stage`. */
export interface BenchmarkProgress {
  stage: BenchmarkSweep
  completed: number
  total: number
}

/** Reports one failed test without ending the stream. */
export interface BenchmarkSweepError {
  sweep: BenchmarkSweep
  message: string
}

// ── Tracker mode ────────────────────────────────────────────────────────────
// Defines tracker sessions for typed events from a real battle.
// These types mirror the tracker DTOs in `poke_rust/src/bin/server/dto.rs`.
// The server converts text to `EventNode` values.
// Tracker and simulator views use the same `BattleView` components.

export interface CreateTrackerRequest {
  /** The tracker viewer's own full roster, as a Showdown teamsheet. */
  myTeam: string
  /** Opponent teamsheet or comma-delimited species names.
   * The server uses only species from this value. */
  opponent: string
  activePerSide: number
  broughtPerSide: number
  statPoints?: boolean
  forceMaxIvs?: boolean
  teraEnabled?: boolean
  megaEnabled?: boolean
  /** Tracker information mode.
   * Tracker mode does not support perfect information. */
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
  /** Complete committed tracker text.
   * An empty string means that no turn is committed. */
  script: string
}

export interface TrackerEventsRequest {
  /** One or more complete turns. Each turn ends with `endofturn`. */
  text: string
}

/** HTTP 422 parse error with the invalid line number. */
export interface TrackerParseError {
  line: number
  message: string
}

export interface TrackerEventsResponse {
  state: BattleView
  /** New committed turns. Append them to the current log. */
  logDelta: TurnLogEntry[]
}

/** Tracker text for an incomplete turn preview.
 * This text does not require `endofturn`. */
export interface TrackerPreviewRequest {
  text: string
}

/** Temporary structural view for an incomplete turn.
 * It contains direct species, move, HP, status, volatile, and boost facts.
 * It does not run the complete inference process or change stored state. */
export interface TrackerPreviewResponse {
  state: BattleView
  /** Parsed preview events with generated guaranteed effects. */
  events: EventNode[]
}

/** Completion names for both match rosters.
 * Moves and abilities come from the roster species.
 * The frontend supplies items from its static list. */
export interface TrackerCompletionsDto {
  species: string[]
  moves: string[]
  abilities: string[]
}

/** Alphabetical display names for all teamsheet species.
 * The setup page uses this list before it creates a session.
 * The server removes battle-only forms. */
export interface SpeciesListDto {
  species: string[]
}
