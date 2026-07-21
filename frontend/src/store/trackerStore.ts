import { create } from 'zustand'
import * as api from '../api/client'
import { TrackerParseApiError } from '../api/client'
import type { BattleView, TurnLogEntry } from '../api/types'
import { megaFormeNames, preloadSprites } from '../lib/sprites'

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

interface TrackerStore {
  trackerId: string | null
  view: BattleView | null
  log: TurnLogEntry[]
  /** Set from a failed `submitText` — either a parse error (`errorLine` set)
   * or an inference contradiction / network failure (`errorLine` null). */
  error: string | null
  errorLine: number | null
  busy: boolean

  create: (req: Parameters<typeof api.createTracker>[0]) => Promise<void>
  restore: (trackerId: string) => Promise<void>
  leave: () => void
  /** Submit tracker-syntax text (one or more complete, `endofturn`-terminated
   * turns). Returns `true` on success so the caller can clear its input. */
  submitText: (text: string) => Promise<boolean>
  clearError: () => void
}

const TRACKER_ID_KEY = 'pokerust.activeTrackerId'

export const useTracker = create<TrackerStore>((set, get) => ({
  trackerId: null,
  view: null,
  log: [],
  error: null,
  errorLine: null,
  busy: false,

  create: async (req) => {
    set({ busy: true, error: null, errorLine: null })
    try {
      const response = await api.createTracker(req)
      sessionStorage.setItem(TRACKER_ID_KEY, response.trackerId)
      preloadTrackerSprites(response.state)
      set({ trackerId: response.trackerId, view: response.state, log: [], busy: false })
    } catch (err) {
      set({ error: err instanceof Error ? err.message : String(err), busy: false })
    }
  },

  restore: async (trackerId) => {
    try {
      const response = await api.getTracker(trackerId)
      preloadTrackerSprites(response.state)
      set({ trackerId, view: response.state, log: response.log })
    } catch {
      sessionStorage.removeItem(TRACKER_ID_KEY)
    }
  },

  leave: () => {
    sessionStorage.removeItem(TRACKER_ID_KEY)
    set({ trackerId: null, view: null, log: [], error: null, errorLine: null })
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
        busy: false,
      }))
      return true
    } catch (err) {
      const line = err instanceof TrackerParseApiError ? err.line : null
      set({
        error: err instanceof Error ? err.message : String(err),
        errorLine: line,
        busy: false,
      })
      return false
    }
  },

  clearError: () => set({ error: null, errorLine: null }),
}))

export function storedTrackerId(): string | null {
  return sessionStorage.getItem(TRACKER_ID_KEY)
}
