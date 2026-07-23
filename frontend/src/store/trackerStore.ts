import { create } from 'zustand'
import * as api from '../api/client'
import { TrackerParseApiError } from '../api/client'
import type { BattleView, EventNode, TurnLogEntry } from '../api/types'
import { CATALOG } from '../lib/items'
import { megaFormeNames, preloadSprites } from '../lib/sprites'
import { contentLinesOf, splitScriptIntoTurns, type CompletionPools } from '../lib/trackerGrammar'

/** Warm the sprite caches for every mon currently visible in the tracker —
 * mirrors `battleStore.ts`'s `preloadBattleSprites`, minus the team-preview
 * bucket (tracker mode has no team-preview phase of its own — see the
 * server's `tracker.rs` module doc for why). */
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
  /** One raw-text chunk per COMMITTED turn (derived from the server's
   * newline-joined `script`) — the frontend's source of truth for "what has
   * this tracker session actually recorded," since `log` alone can't be
   * turned back into tracker syntax for editing. `TrackerInputBar` reads this
   * for ArrowUp/ArrowDown turn navigation. */
  committedTurns: string[]
  /** Autocomplete name pools for the input bar: species/moves/abilities come
   * from `GET /completions` (match-scoped — see that DTO's doc comment);
   * items come straight from the existing static catalog (not
   * match-scoped, so no round trip needed). */
  completions: CompletionPools
  /** Live Pass-1-only structural view of the in-progress (uncommitted) turn —
   * see `TrackerPreviewResponse`'s doc comment. `null` when there's no draft
   * to preview; the input bar/arena should render `previewView ?? view`. */
  previewView: BattleView | null
  /** The in-progress turn's parsed events, for rendering "in progress" log
   * lines the same way a committed turn's events render. */
  previewEvents: EventNode[]
  /** Set from a failed submission — either a parse error (`errorLine` set)
   * or an inference contradiction / network failure (`errorLine` null). */
  error: string | null
  errorLine: number | null
  busy: boolean

  create: (req: Parameters<typeof api.createTracker>[0]) => Promise<void>
  restore: (trackerId: string) => Promise<void>
  leave: () => void
  /** Legacy path: submit tracker-syntax text (one or more complete,
   * `endofturn`-terminated turns) directly, bypassing the draft/preview flow.
   * Kept for scripts/tests that want to seed several turns of history in one
   * call; `TrackerInputBar` itself drives `previewDraft`/`endTurn` instead. */
  submitText: (text: string) => Promise<boolean>
  /** Per-event structural preview of the in-progress turn's lines so far
   * (Pass 1 only — safe on a partial turn, never mutates committed state).
   * Called once per committed event line, not per keystroke. */
  previewDraft: (lines: string[]) => Promise<void>
  /** Clear the live preview (e.g. once the draft is emptied). */
  clearPreview: () => void
  /** Commit `lines` as a complete turn: appends `endofturn` and rebuilds the
   * WHOLE script via `PUT /history` (so a turn reopened by
   * `popLastCommittedTurn` recomputes in place rather than duplicating).
   * Returns `true` on success. */
  endTurn: (lines: string[]) => Promise<boolean>
  /** Correct a single already-committed line (`lineIndex` within
   * `committedTurns[turnIndex]`, 0-based over that turn's content lines —
   * see `contentLinesOf`) and rebuild the whole script with it replaced. This
   * is how editing a PAST event — from any turn, not just the latest — takes
   * effect and recomputes the belief; see `rebuild_tracker_history`'s doc
   * comment on the Rust side for why a full rebuild (not a targeted patch) is
   * required. Returns `true` on success. */
  editCommitted: (turnIndex: number, lineIndex: number, newText: string) => Promise<boolean>
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
    // Autocomplete is a convenience, not a correctness gate — a failed fetch
    // just means suggestions are empty until the next successful load; typed
    // text still round-trips through the real parser on submit either way.
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
    const { trackerId } = get()
    if (!trackerId || lines.length === 0) {
      set({ previewView: null, previewEvents: [] })
      return
    }
    try {
      const response = await api.previewTrackerEvents(trackerId, { text: lines.join('\n') })
      set({ previewView: response.state, previewEvents: response.events })
    } catch {
      // A partial, still-being-typed line can easily fail to parse mid-word —
      // that's expected and not an error worth surfacing; just hold the last
      // good preview rather than clearing it out from under the user.
    }
  },

  clearPreview: () => set({ previewView: null, previewEvents: [] }),

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
        busy: false,
      })
      return true
    } catch (err) {
      const line = err instanceof TrackerParseApiError ? err.line : null
      set({ error: err instanceof Error ? err.message : String(err), errorLine: line, busy: false })
      return false
    }
  },

  clearError: () => set({ error: null, errorLine: null }),
}))

export function storedTrackerId(): string | null {
  return sessionStorage.getItem(TRACKER_ID_KEY)
}
