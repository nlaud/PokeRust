import { create } from 'zustand'
import * as api from '../api/client'
import type { SolveProgress, SolveRequest, SolveStarted, SolveUpdate } from '../api/types'

/** Where one job stands.
 *
 * `off` means that no job ran yet. `cancelled` means that a request or a
 * committed turn stopped the job. */
export type SolvePhase = 'off' | 'starting' | 'running' | 'done' | 'failed' | 'cancelled'

interface SolveStore {
  phase: SolvePhase
  jobId: string | null
  /** The position, the profile, and the depth horizon of the running job. */
  started: SolveStarted | null
  /** The current simulation count and its hard limit. */
  progress: SolveProgress | null
  /** The newest complete answer.
   *
   * It stays on screen while the next depth runs, and it stays after the job
   * ends. Only a deeper complete answer replaces it. */
  complete: SolveUpdate | null
  /** The complete answer before `complete`.
   *
   * The panel compares the two to show the value stability and the support
   * change. */
  previousComplete: SolveUpdate | null
  /** The newest answer of any kind.
   *
   * A double-oracle round sends an answer inside one depth, so this field can
   * be ahead of `complete`. */
  live: SolveUpdate | null
  /** True when the visible answer belongs to an earlier tracker position. */
  stale: boolean
  error: string | null
  /** Registers one job and reads its answers. Replaces any earlier job. */
  start: (req: SolveRequest) => Promise<void>
  /** Stops the running job and keeps every answer on screen. */
  stop: () => Promise<void>
  /** Marks the visible answer as old after the tracker position changes. */
  invalidate: () => void
  /** Clears every answer and stops the running job. */
  reset: () => void
}

/** Cancels the active event stream.
 *
 * The value lives outside the store, because a component unmount must not
 * clear it. */
let cancelStream: (() => void) | null = null

/** Identifies the newest start, stop, invalidate, or reset operation. */
let operation = 0

/** The fields that a new job clears. */
const cleared = {
  progress: null,
  complete: null,
  previousComplete: null,
  live: null,
  stale: false,
  error: null,
}

/** Reads the answers of the streaming solver job.
 *
 * The store keeps the last complete answer while the next depth runs, so the
 * panel never falls back to an empty state between two depths. */
export const useSolve = create<SolveStore>((set, get) => ({
  phase: 'off',
  jobId: null,
  started: null,
  ...cleared,

  start: async (req) => {
    const mine = ++operation
    const oldJobId = get().jobId
    cancelStream?.()
    cancelStream = null
    if (oldJobId !== null) void api.cancelSolve(oldJobId).catch(() => undefined)
    set({ phase: 'starting', jobId: null, started: null, ...cleared })

    let jobId: string
    try {
      jobId = (await api.startSolve(req)).jobId
    } catch (error) {
      if (mine === operation) {
        set({ phase: 'failed', error: error instanceof Error ? error.message : String(error) })
      }
      return
    }
    // A later operation owns the store. Stop this registered job before this
    // request returns, because no event stream will start it.
    if (mine !== operation) {
      await api.cancelSolve(jobId).catch(() => undefined)
      return
    }
    set({ jobId, phase: 'running' })

    const active = () => mine === operation && get().jobId === jobId

    cancelStream = api.streamSolve(jobId, {
      onStarted: (started) => {
        if (active()) {
          set({
            started,
            progress: {
              turnsSimulated: 0,
              simulationTurnBudget: started.profile.simulationTurnBudget,
            },
          })
        }
      },

      onProgress: (progress) => {
        if (active()) set({ progress })
      },

      onUpdate: (update) => {
        if (!active()) return
        set((state) => {
          // Server-Sent Events arrive in order, and the revision never falls.
          // A lower number therefore names an answer of a replaced job.
          if (state.live && update.revision <= state.live.revision) return {}
          if (!update.complete) return { live: update }
          return { live: update, complete: update, previousComplete: state.complete }
        })
      },

      onDone: () => {
        if (active()) set({ phase: 'done' })
      },
      onFailed: (failed) => {
        if (active()) set({ phase: 'failed', error: failed.message })
      },
      onCancelled: () => {
        if (active()) set({ phase: 'cancelled', stale: get().complete !== null })
      },
      onAborted: (message) => {
        if (active()) set({ phase: 'failed', error: message })
      },
    })
  },

  stop: async () => {
    operation += 1
    const jobId = get().jobId
    cancelStream?.()
    cancelStream = null
    set({ phase: 'cancelled', jobId: null })
    if (jobId === null) return
    try {
      await api.cancelSolve(jobId)
    } catch {
      // The job already ended, which is the same outcome.
    }
  },

  invalidate: () => {
    operation += 1
    const jobId = get().jobId
    cancelStream?.()
    cancelStream = null
    if (jobId !== null) void api.cancelSolve(jobId).catch(() => undefined)
    set({ phase: 'cancelled', jobId: null, stale: get().complete !== null, error: null })
  },

  reset: () => {
    operation += 1
    const jobId = get().jobId
    cancelStream?.()
    cancelStream = null
    if (jobId !== null) void api.cancelSolve(jobId).catch(() => undefined)
    set({ phase: 'off', jobId: null, started: null, ...cleared })
  },
}))
