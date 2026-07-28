import { create } from 'zustand'
import * as api from '../api/client'
import type { BenchmarkProgress, InferenceRow, SolverRow, TurnSpeedRow } from '../api/types'

/** One sweep's lifecycle. Tracked per sweep rather than globally because the
 * three run sequentially server-side and finish at wildly different times — the
 * solver sweep takes minutes longer than turn speed — so the page renders each
 * chart the moment its own sweep lands. */
export type SweepStatus = 'idle' | 'running' | 'done' | 'failed'

interface SweepState<Row> {
  status: SweepStatus
  rows: Row[]
  /** Latest progress event for this sweep; `null` before its first one. */
  progress: BenchmarkProgress | null
  error: string | null
}

function idleSweep<Row>(): SweepState<Row> {
  return { status: 'idle', rows: [], progress: null, error: null }
}

interface BenchmarkStore {
  turnSpeed: SweepState<TurnSpeedRow>
  inference: SweepState<InferenceRow>
  solver: SweepState<SolverRow>
  /** True while the stream is open, i.e. at least one sweep may still report. */
  busy: boolean
  /** Whole-stream failure (connection lost), as opposed to one sweep failing. */
  streamError: string | null
  /** Start a new run, discarding any previous results. Safe to call while a
   * previous stream is still open — it cancels that one first. */
  run: () => void
}

/** The SSE connection's cancel handle. Lives outside the store (not state)
 * since it's an imperative handle, not data to render. Module-level so it, like
 * the store itself, survives the page component unmounting on a tab switch. */
let cancelStream: (() => void) | null = null

/** Benchmark run state as a page-independent singleton store, not page-local
 * `useState`. `BenchmarkingPage` sits behind a react-router `<Route>` (see
 * `App.tsx`) and is unmounted on tab switch, which would otherwise discard
 * results and tear down the in-flight stream. Zustand stores are module
 * singletons (same pattern as `trackerStore.ts` / `battleStore.ts`), so results
 * and an in-flight run survive navigating away and back — only an explicit
 * `run()` resets them. */
export const useBenchmark = create<BenchmarkStore>((set) => ({
  turnSpeed: idleSweep(),
  inference: idleSweep(),
  solver: idleSweep(),
  busy: false,
  streamError: null,

  run: () => {
    cancelStream?.()
    set({
      turnSpeed: { ...idleSweep<TurnSpeedRow>(), status: 'running' },
      inference: { ...idleSweep<InferenceRow>(), status: 'running' },
      solver: { ...idleSweep<SolverRow>(), status: 'running' },
      busy: true,
      streamError: null,
    })

    cancelStream = api.streamBenchmark({
      onProgress: (progress) =>
        set((state) => ({
          [progress.stage]: { ...state[progress.stage], progress },
        })),

      onResult: (result) =>
        set((state) => ({
          [result.sweep]: {
            ...state[result.sweep],
            status: 'done',
            rows: result.rows,
          },
        })),

      onSweepFailed: (failure) =>
        set((state) => ({
          [failure.sweep]: {
            ...state[failure.sweep],
            status: 'failed',
            error: failure.message,
          },
        })),

      // `done` is terminal even if a worker failed before it could emit its
      // per-sweep event. Never leave a card spinning after the stream closes.
      onDone: () =>
        set((state) => {
          const finish = <Row,>(sweep: SweepState<Row>): SweepState<Row> =>
            sweep.status === 'running'
              ? { ...sweep, status: 'failed', error: 'Benchmark ended before this sweep reported' }
              : sweep
          return {
            busy: false,
            turnSpeed: finish(state.turnSpeed),
            inference: finish(state.inference),
            solver: finish(state.solver),
          }
        }),

      // The stream died, so anything still running will never report. Mark
      // those failed rather than leaving them spinning forever; sweeps that
      // already landed keep their results.
      onAborted: (message) =>
        set((state) => {
          const abort = <Row,>(sweep: SweepState<Row>): SweepState<Row> =>
            sweep.status === 'running' ? { ...sweep, status: 'failed', error: message } : sweep
          return {
            busy: false,
            streamError: message,
            turnSpeed: abort(state.turnSpeed),
            inference: abort(state.inference),
            solver: abort(state.solver),
          }
        }),
    })
  },
}))
