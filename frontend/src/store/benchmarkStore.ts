import { create } from 'zustand'
import * as api from '../api/client'
import type { BenchmarkProgress, InferenceRow, SolverRow, TurnSpeedRow } from '../api/types'

/** Stores the state for one benchmark test.
 * Each test can show its chart when its result arrives. */
export type SweepStatus = 'idle' | 'running' | 'done' | 'failed'

interface SweepState<Row> {
  status: SweepStatus
  rows: Row[]
  /** Latest progress event, or `null` before the first event. */
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
  /** True while the stream can report another test. */
  busy: boolean
  /** Error that closed the complete stream. */
  streamError: string | null
  /** Cancels the old stream and starts a new benchmark. */
  run: () => void
}

/** Cancels the active event stream.
 * The module value remains available after a page unmount. */
let cancelStream: (() => void) | null = null

/** Stores benchmark state outside the page component.
 * Results and the active stream remain after a route change.
 * Only `run` clears the old results. */
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

      // Treat `done` as the terminal event for all tests.
      // Stop any card that still shows progress.
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

      // Mark each active test as failed after a stream failure.
      // Keep results from completed tests.
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
