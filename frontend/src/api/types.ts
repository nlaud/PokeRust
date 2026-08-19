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
 * The last four names sample, so they return an estimate. */
export type BotAlgorithm =
  | 'backwardInduction'
  | 'serializedBounds'
  | 'doubleOracle'
  | 'mcts'
  | 'ismcts'
  | 'mccfr'
  | 'pimc'

/** The optional solver profile that plays P2.
 * Every field is optional. The server supplies each absent field.
 * The server rejects a limit that the algorithm cannot read. */
export interface BotProfileRequest {
  preset?: 'fast' | 'balanced' | 'competitive' | 'custom'
  /** As `botP2`, an absent value takes the search of the information mode.
   * A fog-of-war mode takes `ismcts`, and Perfect Information takes
   * `doubleOracle`. The response reports the name in `botP2.algorithm`.
   * The tracker analysis endpoint takes `ismcts`, `mccfr`, or `pimc`. */
  algorithm?: BotAlgorithm
  /** The maximum number of uncached turns that the full search can simulate. */
  simulationTurnBudget?: number
  depth?: number
  /** Turns of lookahead below a replacement or a self-switch pivot.
   * An absent field gives a forced decision the remaining turn depth. */
  replacementDepth?: number
  /** The number of damage rolls in solver simulations. */
  damageRolls?: number
  /** Enables critical-hit branches in solver simulations. */
  considerCrit?: boolean
  /** Belief searches only. */
  particles?: number
  /** Makes a sampled search reproducible.
   * The maximum value is `Number.MAX_SAFE_INTEGER`. */
  seed?: number
  /** Kept for compatibility. Bot sessions always show the strategy. */
  revealStrategy?: boolean
}

/** The resolved profile that the server returns. */
export interface BotProfileView {
  algorithm: BotAlgorithm
  /** True when the algorithm itself is exact.
   * A limit can still make the result approximate. */
  exact: boolean
  simulationTurnBudget: number
  depth: number
  damageRolls: number
  considerCrit: boolean
  /** Null when a forced decision uses the remaining turn budget. */
  replacementDepth: number | null
  /** The workers that this search asks the process pool for.
   *
   * A busy pool can give the search fewer workers. The count does not change
   * the value or either strategy. */
  workers: number
  particles: number | null
  seed: number | null
  /** Always true for a bot profile. */
  revealStrategy: boolean
  /** Each reason that the result can differ from the exact answer. */
  approximations: string[]
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

// ── Strategy rows ───────────────────────────────────────────────────────────
// A strategy row renders one joint action of one mixed strategy. The tracker
// panel shows both players. A bot battle shows Player 2.

/** One bring-and-lead choice of a team-preview strategy. */
export interface PreviewChoice {
  /** The lead species, in slot order. */
  leads: string[]
  /** The other brought species, in roster order. */
  back: string[]
}

/** One joint action of a strategy, with its rate. */
export interface StrategyRow {
  /** One command for each active slot, in slot order.
   * A team-preview row holds no command. */
  commands: CommandOption[]
  /** The bring-and-lead choice of a team-preview row.
   * `null` in a battle row. */
  preview: PreviewChoice | null
  /** How often the strategy plays this joint action, from 0 through 1. */
  probability: number
}

/** Player 2's mixed strategy at one battle position. */
export interface P2Strategy {
  /** Which question this block answers.
   * A `teamPreview` block answers the bring-and-lead choice. */
  position: 'battle' | 'teamPreview'
  /** All positive-rate rows, highest first.
   * A row with a rate of zero does not appear. */
  rows: StrategyRow[]
  /** How many positive-rate rows the strategy holds. */
  total: number
  /** The index in `rows` of the row that supplied the drawn command.
   *
   * Absent in a checkpoint block, which names no draw. */
  drawnIndex?: number
}

/** The drawn P2 command of one bot turn.
 *
 * The reveal carries one action and nothing else of P2's plan: no probability
 * of that action, no second action, and no win odds. The server returns it only
 * with the resolved turn, so both commands are already locked.
 *
 * A bot session also carries the strategy that supplied the draw. */
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
  /** The strategy that the draw sampled from.
   *
   * Absent for a hotseat or uniform draw. */
  strategy?: P2Strategy
}

/** The data that repeats one analysis search. */
export interface AnalysisReplay {
  /** These values identify the position in the session history. */
  generation: number
  turnNumber: number
  /** The seed of the search. */
  searchSeed: number
  algorithm: string
  simulationTurnBudget: number
  depth: number
  /** Null when a forced decision uses the remaining turn budget. */
  replacementDepth: number | null
  workers: number
  particles: number | null
  damageRolls: number
  considerCrit: boolean
  turnsSimulated: number
}

/** The phase of the P2 analysis job. */
export type AnalysisPhase = 'idle' | 'running' | 'complete' | 'failed'

/** The cost of one complete analysis job. */
export interface AnalysisCheckpoint {
  /** The generation of the position that the search read. */
  generation: number
  /** True when a later state change made this answer old. */
  stale: boolean
  turnNumber: number
  /** The depth that the search reached. */
  depthReached: number
  /** True when the solver completed this depth. */
  complete: boolean
  /** Player 2's current estimated win rate. */
  p2WinOdds: number
  elapsedMs: number
  /** The uncached turns that the full search simulated. */
  turnsSimulated: number
  /** The seed of this search, which makes the result reproducible.
   * The server draws it below `Number.MAX_SAFE_INTEGER`, so it round-trips. */
  seed: number
  /** Every reason that the answer is approximate. */
  warnings: string[]
  /** Player 2's strategy at the position that this checkpoint answers.
   *
   * Absent for a stale checkpoint because its actions belong to an older
   * position. */
  p2Strategy?: P2Strategy
}

/** The progress of the P2 analysis job.
 *
 * A bot session includes the newest P2 strategy and win estimate. A hotseat
 * session has no bot analysis. */
export interface AnalysisProgressResponse {
  /** The generation of the current position.
   * Every state change raises it by one. */
  generation: number
  phase: AnalysisPhase
  /** How long the running job has run, or null when no job runs. */
  runningMs: number | null
  /** The uncached turns that the running job has claimed. */
  turnsSimulated: number | null
  /** The turn limit of the running job. */
  simulationTurnBudget: number | null
  /** The newest strategy checkpoint.
   * A failure and a cancellation both keep it. */
  checkpoint: AnalysisCheckpoint | null
  /** Why the last job produced no checkpoint. */
  error: string | null
}

// ── Tracker analysis ────────────────────────────────────────────────────────
// The tracker has one user, and that user typed both rosters, so these rows
// carry the strategy and the win odds of both players. The battle endpoints
// keep their own privacy rules — see `AnalysisProgressResponse`.

/** The profile that starts a tracker search.
 * It is the same shape the simulator sends as `botP2`. */
export type TrackerAnalysisRequest = BotProfileRequest

/** The phase of the tracker solver panel.
 * `off` means that the session holds no profile. */
export type TrackerAnalysisPhase = 'off' | 'idle' | 'running' | 'complete' | 'failed'

/** Which question one rung answers. */
export type TrackerAnalysisPosition = 'battle' | 'teamPreview'

/** The rung that the ladder runs now. */
export interface TrackerAnalysisRung {
  /** The depth of the rung that runs. */
  depth: number
  /** The uncached turns that the full search has claimed. */
  turnsSimulated: number
  /** The turn limit of the full search. */
  simulationTurnBudget: number
  /** The claimed turns divided by the turn limit. */
  fraction: number
}

/** The answer of one complete ladder rung. */
export interface TrackerAnalysisCheckpoint {
  /** The generation of the position that the search read. */
  generation: number
  /** True when a later committed turn made this answer old. */
  stale: boolean
  turnNumber: number
  /** The question that this rung answers. */
  position: TrackerAnalysisPosition
  /** The depth of this rung. */
  depthReached: number
  elapsedMs: number
  /** The seed of the draw and the search. */
  seed: number
  /** Player 1's odds of winning, from 0 through 1. */
  p1WinOdds: number
  /** Player 2's odds of winning. The game is zero-sum. */
  p2WinOdds: number
  /** The highest-rate joint actions of Player 1. */
  p1Strategy: StrategyRow[]
  /** The highest-rate joint actions of Player 2. */
  p2Strategy: StrategyRow[]
  /** True when the Player 2 rows form one strategy for one private state. */
  p2StrategyIsPlayable: boolean
  /** Every reason that the answer is approximate. */
  warnings: string[]
}

/** The tracker analysis record of one session. */
export interface TrackerAnalysisResponse {
  /** The generation of the current position.
   * Every committed turn raises it by one. */
  generation: number
  phase: TrackerAnalysisPhase
  /** How long the running ladder has run, or null when no job runs. */
  runningMs: number | null
  /** The configured depth horizon of the ladder. */
  targetDepth: number | null
  /** The rung that runs now.
   * `null` when no job runs, and before the first rung starts. */
  rung: TrackerAnalysisRung | null
  /** The newest strategy checkpoint.
   * A failure and a cancellation both keep it. */
  checkpoint: TrackerAnalysisCheckpoint | null
  /** Player 1's win odds at the position before the current one. */
  previousP1WinOdds: number | null
  /** Why the last job produced no rung. */
  error: string | null
  /** The resolved profile of this session. */
  profile: BotProfileView | null
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

// ── The streaming solve job ─────────────────────────────────────────────────
// `POST /api/solve` registers one job. `GET /api/solve/{id}/events` runs it and
// streams each answer. See `poke_rust/src/bin/server/solve.rs`.

/** Which session a job answers. */
export type SolveSource = 'battle' | 'tracker'

/** The body of `POST /api/solve`. */
export interface SolveRequest {
  source: SolveSource
  /** The battle ID or the tracker ID. */
  sessionId: string
  /** Each absent field takes its server default. */
  profile?: BotProfileRequest
}

/** The answer of `POST /api/solve`. */
export interface SolveJobResponse {
  /** Open `/api/solve/{jobId}/events` to run this job. */
  jobId: string
}

/** The first event of one stream. */
export interface SolveStarted {
  jobId: string
  source: SolveSource
  sessionId: string
  /** A `teamPreview` job answers the bring-and-lead choice. */
  position: 'battle' | 'teamPreview'
  /** The position counter of the position that this job reads. */
  generation: number
  /** The seed of the draw and the search.
   *
   * A profile with no seed gets a random one. The client reads this number, so
   * the same answer can be searched again. */
  seed: number
  /** The depth horizon of the ladder. */
  targetDepth: number
  profile: BotProfileView
}

/** The live simulation count of one search. */
export interface SolveProgress {
  turnsSimulated: number
  simulationTurnBudget: number
}

/** What one search cost.
 * A sampling search builds no matrix, so every matrix counter is zero. */
export interface SolveStats {
  nodesExpanded: number
  turnsSimulated: number
  matrixCellsEvaluated: number
  matrixCellsTotal: number
  lpsSolved: number
  abCutoffs: number
  ttHits: number
  turnCacheHits: number
}

/** The sampling detail of one approximate answer.
 * An exact search sends none. */
export interface SolveSampling {
  algorithm: 'mcts' | 'ismcts' | 'mccfr' | 'pimc'
  /** The iterations that the search finished. */
  iterations: number
  /** The worlds that a belief search drew.
   * Null for a search of one concrete position. */
  particles: number | null
  seed: number
  /** The leaf evaluator that scored the depth horizon. */
  evaluator: string
}

/** One published answer of a running job. */
export interface SolveUpdate {
  generation: number
  /** The count of answers that this job sent before this one.
   * It starts at zero, and it never falls. */
  revision: number
  depth: number
  depthTarget: number
  elapsedMs: number
  /** True when this answer ends a depth.
   * A false value marks one double-oracle round inside a depth. */
  complete: boolean
  value: number
  p1WinOdds: number
  p2WinOdds: number
  /** The complete mixed strategy of Player 1. */
  p1Strategy: StrategyRow[]
  /** The complete mixed strategy of Player 2.
   *
   * A tracker job always sends the rows. */
  p2Strategy: StrategyRow[] | null
  /** True when the Player 2 rows form one strategy for one private state. */
  p2StrategyIsPlayable: boolean
  stats: SolveStats
  /** Null for an exact search. */
  sampling: SolveSampling | null
  /** Every reason that this answer is approximate. */
  warnings: string[]
}

/** The last event of a job that finished its ladder. */
export interface SolveDone {
  jobId: string
  updates: number
}

/** The last event of a job that produced no result. */
export interface SolveFailed {
  jobId: string
  message: string
}

/** The last event of a job that a request or a new position stopped. */
export interface SolveCancelled {
  jobId: string
  reason: string
}
