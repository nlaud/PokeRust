import { create } from 'zustand'
import * as api from '../api/client'
import { TrackerParseApiError } from '../api/client'
import type { BattleView, EventNode, TurnLogEntry } from '../api/types'
import { CATALOG } from '../lib/items'
import { megaFormeNames, preloadSprites } from '../lib/sprites'
import { contentLinesOf, splitScriptIntoTurns, type CompletionPools } from '../lib/trackerGrammar'

/** Loads sprites for each Pokémon in the current tracker view. */
function preloadTrackerSprites(view: BattleView) {
  const mons = [
    ...(view.p1 ? [...view.p1.active, ...view.p1.back] : []),
    ...(view.p2 ? [...view.p2.active, ...view.p2.back] : []),
  ]
  preloadSprites(mons.flatMap((m) => [m.species, ...megaFormeNames(m.species, m.item)]))
}

const ITEM_LABELS = CATALOG.map((c) => c.label)

interface TrackerStore {
  trackerId: string | null
  view: BattleView | null
  log: TurnLogEntry[]
  /** Raw tracker text for each committed turn.
   * The input bar uses this text for editing and turn navigation. */
  committedTurns: string[]
  /** Match-specific species, move, and ability suggestions.
   * Item suggestions use the static item list. */
  completions: CompletionPools
  /** Structural view of the uncommitted turn.
   * `null` means that no draft exists. */
  previewView: BattleView | null
  /** Parsed events for the uncommitted turn. */
  previewEvents: EventNode[]
  /** Warning that the last draft change had no visible effect.
   * The comparison uses complete event trees because the server can merge lines.
   * This warning does not block submission. */
  lastLineWarning: string | null
  /** Submission error.
   * `errorLine` identifies a parse error line when available. */
  error: string | null
  errorLine: number | null
  busy: boolean

  create: (req: Parameters<typeof api.createTracker>[0]) => Promise<void>
  restore: (trackerId: string) => Promise<void>
  leave: () => void
  /** Submits one or more complete turns without the draft process.
   * Scripts and tests use this method to add history. */
  submitText: (text: string) => Promise<boolean>
  /** Previews direct structural facts from the current draft.
   * This method does not change committed state. */
  previewDraft: (lines: string[]) => Promise<void>
  /** Clears the live preview. */
  clearPreview: () => void
  /** Adds `endofturn` and rebuilds history with the completed turn.
   * Returns `true` after a successful commit. */
  endTurn: (lines: string[]) => Promise<boolean>
  /** Replaces one committed line and rebuilds all tracker history.
   * The rebuild recalculates beliefs after the edit.
   * Returns `true` after a successful edit. */
  editCommitted: (turnIndex: number, lineIndex: number, newText: string) => Promise<boolean>
  /** Removes the last committed turn and rebuilds server history.
   * The returned lines can refill the draft for editing.
   * Returns `null` when no turn exists or the rebuild fails. */
  popLastCommittedTurn: () => Promise<string[] | null>
  clearError: () => void
}

const TRACKER_ID_KEY = 'pokerust.activeTrackerId'

function replaceLineInTurn(turnText: string, lineIndex: number, newText: string): string {
  const content = contentLinesOf(turnText)
  if (lineIndex < 0 || lineIndex >= content.length) return turnText
  content[lineIndex] = newText
  return [...content, 'endofturn'].join('\n')
}

async function loadCompletions(trackerId: string): Promise<CompletionPools> {
  try {
    const dto = await api.getTrackerCompletions(trackerId)
    return { species: dto.species, moves: dto.moves, abilities: dto.abilities, items: ITEM_LABELS }
  } catch {
    // Keep suggestions empty until the next successful load.
    // The server still validates typed text.
    return { species: [], moves: [], abilities: [], items: ITEM_LABELS }
  }
}

export const useTracker = create<TrackerStore>((set, get) => ({
  trackerId: null,
  view: null,
  log: [],
  committedTurns: [],
  completions: { species: [], moves: [], abilities: [], items: ITEM_LABELS },
  previewView: null,
  previewEvents: [],
  lastLineWarning: null,
  error: null,
  errorLine: null,
  busy: false,

  create: async (req) => {
    set({ busy: true, error: null, errorLine: null })
    try {
      const response = await api.createTracker(req)
      sessionStorage.setItem(TRACKER_ID_KEY, response.trackerId)
      preloadTrackerSprites(response.state)
      const completions = await loadCompletions(response.trackerId)
      set({
        trackerId: response.trackerId,
        view: response.state,
        log: [],
        committedTurns: [],
        completions,
        previewView: null,
        previewEvents: [],
        lastLineWarning: null,
        busy: false,
      })
    } catch (err) {
      set({ error: err instanceof Error ? err.message : String(err), busy: false })
    }
  },

  restore: async (trackerId) => {
    try {
      const response = await api.getTracker(trackerId)
      preloadTrackerSprites(response.state)
      const completions = await loadCompletions(trackerId)
      set({
        trackerId,
        view: response.state,
        log: response.log,
        committedTurns: splitScriptIntoTurns(response.script),
        completions,
      })
    } catch {
      sessionStorage.removeItem(TRACKER_ID_KEY)
    }
  },

  leave: () => {
    sessionStorage.removeItem(TRACKER_ID_KEY)
    set({
      trackerId: null,
      view: null,
      log: [],
      committedTurns: [],
      previewView: null,
      previewEvents: [],
      lastLineWarning: null,
      error: null,
      errorLine: null,
    })
  },

  submitText: async (text) => {
    const { trackerId } = get()
    if (!trackerId) return false
    set({ busy: true, error: null, errorLine: null })
    try {
      const response = await api.postTrackerEvents(trackerId, { text })
      preloadTrackerSprites(response.state)
      set((s) => ({
        view: response.state,
        log: [...s.log, ...response.logDelta],
        committedTurns: [...s.committedTurns, ...splitScriptIntoTurns(text)],
        busy: false,
      }))
      return true
    } catch (err) {
      const line = err instanceof TrackerParseApiError ? err.line : null
      set({ error: err instanceof Error ? err.message : String(err), errorLine: line, busy: false })
      return false
    }
  },

  previewDraft: async (lines) => {
    const { trackerId, previewEvents: eventsBefore } = get()
    if (!trackerId || lines.length === 0) {
      set({ previewView: null, previewEvents: [], lastLineWarning: null })
      return
    }
    try {
      const response = await api.previewTrackerEvents(trackerId, { text: lines.join('\n') })
      // Compare the serialized event trees before and after the draft change.
      // Equal trees mean that the change had no visible effect.
      // Compare trees instead of lines because the server can merge related lines.
      const unchanged = JSON.stringify(response.events) === JSON.stringify(eventsBefore)
      set({
        previewView: response.state,
        previewEvents: response.events,
        lastLineWarning: unchanged ? 'That line had no visible effect.' : null,
      })
    } catch {
      // A partial word can fail to parse.
      // Keep the last valid preview.
    }
  },

  clearPreview: () => set({ previewView: null, previewEvents: [], lastLineWarning: null }),

  endTurn: async (lines) => {
    const { trackerId, committedTurns } = get()
    if (!trackerId || lines.length === 0) return false
    set({ busy: true, error: null, errorLine: null })
    const turnText = [...lines, 'endofturn'].join('\n')
    const fullScript = [...committedTurns, turnText].join('\n')
    try {
      const response = await api.rebuildTrackerHistory(trackerId, { text: fullScript })
      preloadTrackerSprites(response.state)
      set({
        view: response.state,
        log: response.log,
        committedTurns: splitScriptIntoTurns(response.script),
        previewView: null,
        previewEvents: [],
        lastLineWarning: null,
        busy: false,
      })
      return true
    } catch (err) {
      const line = err instanceof TrackerParseApiError ? err.line : null
      set({ error: err instanceof Error ? err.message : String(err), errorLine: line, busy: false })
      return false
    }
  },

  editCommitted: async (turnIndex, lineIndex, newText) => {
    const { trackerId, committedTurns } = get()
    if (!trackerId || turnIndex < 0 || turnIndex >= committedTurns.length) return false
    set({ busy: true, error: null, errorLine: null })
    const corrected = [...committedTurns]
    corrected[turnIndex] = replaceLineInTurn(corrected[turnIndex], lineIndex, newText)
    try {
      const response = await api.rebuildTrackerHistory(trackerId, { text: corrected.join('\n') })
      preloadTrackerSprites(response.state)
      set({
        view: response.state,
        log: response.log,
        committedTurns: splitScriptIntoTurns(response.script),
        previewView: null,
        previewEvents: [],
        lastLineWarning: null,
        busy: false,
      })
      return true
    } catch (err) {
      const line = err instanceof TrackerParseApiError ? err.line : null
      set({ error: err instanceof Error ? err.message : String(err), errorLine: line, busy: false })
      return false
    }
  },

  popLastCommittedTurn: async () => {
    const { trackerId, committedTurns } = get()
    if (!trackerId || committedTurns.length === 0) return null
    const popped = committedTurns[committedTurns.length - 1]
    const remaining = committedTurns.slice(0, -1)
    set({ busy: true, error: null, errorLine: null })
    try {
      const response = await api.rebuildTrackerHistory(trackerId, { text: remaining.join('\n') })
      preloadTrackerSprites(response.state)
      set({
        view: response.state,
        log: response.log,
        committedTurns: splitScriptIntoTurns(response.script),
        previewView: null,
        previewEvents: [],
        lastLineWarning: null,
        busy: false,
      })
      return contentLinesOf(popped)
    } catch (err) {
      const line = err instanceof TrackerParseApiError ? err.line : null
      set({ error: err instanceof Error ? err.message : String(err), errorLine: line, busy: false })
      return null
    }
  },

  clearError: () => set({ error: null, errorLine: null }),
}))

export function storedTrackerId(): string | null {
  return sessionStorage.getItem(TRACKER_ID_KEY)
}
