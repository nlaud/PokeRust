import { create } from 'zustand'
import * as api from '../api/client'
import type { BenchmarkProgress, BenchmarkResponse } from '../api/types'

interface BenchmarkStore {
  data: BenchmarkResponse | null
  busy: boolean
  progress: BenchmarkProgress | null
  error: string | null
  /** Start a new sweep, replacing any previous result. Safe to call while a
   * previous stream is still open (e.g. a stale click) — it cancels that one
   * first. */
  run: () => void
}

/** The SSE connection's cancel handle. Lives outside the store (not state)
 * since it's an imperative handle, not data to render — mirrors the
 * `cancelRef` pattern `BenchmarkingPage` used to keep locally. Module-level so
 * it, like the store itself, survives the page component unmounting when the
 * user switches tabs. */
let cancelStream: (() => void) | null = null

/** Benchmark run state as a page-independent singleton store, not
 * page-local `useState`. `BenchmarkingPage` sits behind a react-router
 * `<Route>` (see `App.tsx`) and is unmounted on tab switch, which used to
 * discard `data`/`progress` and tear down the in-flight stream. Zustand
 * stores are module singletons (same pattern as `trackerStore.ts` /
 * `battleStore.ts`), so results and an in-flight run now survive navigating
 * away and back — only an explicit `run()` call resets them. */
export const useBenchmark = create<BenchmarkStore>((set) => ({
  data: null,
  busy: false,
  progress: null,
  error: null,

  run: () => {
    cancelStream?.()
    set({ busy: true, error: null, progress: null, data: null })
    cancelStream = api.streamBenchmark({
      onProgress: (progress) => set({ progress }),
      onResult: (result) => set({ data: result, busy: false }),
      onFailed: (message) => set({ error: message, busy: false }),
    })
  },
}))
