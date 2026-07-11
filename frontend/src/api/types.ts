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

export interface NamedTurns {
  name: string
  turns?: number
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

/** `'perfect'` (default) | `'openSheet'` | `'openSheetNatures'`. Selects the
 * fog-of-war starting baseline for P1's view of P2 — see `InformationMode` in the
 * Rust engine. */
export type InformationMode = 'perfect' | 'openSheet' | 'openSheetNatures'

export interface CreateBattleRequest {
  p1Team: string
  p2Team: string
  activePerSide: number
  broughtPerSide: number
  statPoints?: boolean
  considerCrit?: boolean
  /** Pin all opponent IVs to 31 for the fog-of-war inference engine (Champions
   * competitive default). Mirrors `InferenceConfig::force_max_ivs` in the Rust engine. */
  forceMaxIvs?: boolean
  damageRolls?: number
  informationMode?: InformationMode
}

export interface CreateBattleResponse {
  battleId: string
  state: BattleView
}

export interface GetBattleResponse {
  state: BattleView
  log: TurnLogEntry[]
}

export interface TurnRequest {
  p1: PlayerCommand
  p2: PlayerCommand
}

export interface TurnResponse {
  state: BattleView
  events: EventNode[]
  probability: number
}
