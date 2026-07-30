import type {
  BenchmarkProgress,
  BenchmarkResult,
  BenchmarkSweepError,
  CreateBattleRequest,
  CreateBattleResponse,
  CreateTrackerRequest,
  CreateTrackerResponse,
  GetBattleResponse,
  GetTrackerResponse,
  LegalCommands,
  PlayerId,
  SpeciesListDto,
  TrackerCompletionsDto,
  TrackerEventsRequest,
  TrackerEventsResponse,
  TrackerPreviewRequest,
  TrackerPreviewResponse,
  TurnRequest,
  TurnResponse,
} from './types'

export class ApiError extends Error {
  status: number
  constructor(status: number, message: string) {
    super(message)
    this.status = status
  }
}

/** Thrown by `postTrackerEvents` on a parse failure — `line` (1-based) lets the
 * caller point the editor at the offending line instead of just showing text. */
export class TrackerParseApiError extends ApiError {
  line: number
  constructor(line: number, message: string) {
    super(422, message)
    this.line = line
  }
}

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const response = await fetch(path, {
    headers: { 'Content-Type': 'application/json' },
    ...init,
  })
  if (!response.ok) {
    let message = response.statusText
    try {
      const body = await response.json()
      if (body.message) message = body.message
    } catch {
      // Keep the status text for a non-JSON error body.
    }
    throw new ApiError(response.status, message)
  }
  if (response.status === 204) return undefined as T
  return response.json() as Promise<T>
}

export function createBattle(req: CreateBattleRequest): Promise<CreateBattleResponse> {
  return request('/api/battles', { method: 'POST', body: JSON.stringify(req) })
}

export function getBattle(battleId: string): Promise<GetBattleResponse> {
  return request(`/api/battles/${battleId}`)
}

export function getCommands(battleId: string, player: PlayerId): Promise<LegalCommands> {
  return request(`/api/battles/${battleId}/commands?player=${player}`)
}

export function submitTurn(battleId: string, req: TurnRequest): Promise<TurnResponse> {
  return request(`/api/battles/${battleId}/turn`, {
    method: 'POST',
    body: JSON.stringify(req),
  })
}

export function deleteBattle(battleId: string): Promise<void> {
  return request(`/api/battles/${battleId}`, { method: 'DELETE' })
}

// ── Tracker mode ────────────────────────────────────────────────────────────

export function createTracker(req: CreateTrackerRequest): Promise<CreateTrackerResponse> {
  return request('/api/tracker', { method: 'POST', body: JSON.stringify(req) })
}

export function getTracker(trackerId: string): Promise<GetTrackerResponse> {
  return request(`/api/tracker/${trackerId}`)
}

export function deleteTracker(trackerId: string): Promise<void> {
  return request(`/api/tracker/${trackerId}`, { method: 'DELETE' })
}

/** Shared by every tracker-TEXT endpoint (`/events`, `/preview`, `/history`):
 * a parse failure's body shape is `{ line, message }` (see
 * `TrackerParseApiError`), not the ordinary `{ message }` `ApiError` body
 * every other endpoint returns — so this can't just be the generic `request`
 * helper above. */
async function requestTrackerText<T>(path: string, method: string, body: unknown): Promise<T> {
  const response = await fetch(path, {
    method,
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body),
  })
  if (!response.ok) {
    let parsed: { line?: number; message?: string } = {}
    try {
      parsed = await response.json()
    } catch {
      // Use the generic message for a non-JSON error body.
    }
    if (typeof parsed.line === 'number') {
      throw new TrackerParseApiError(parsed.line, parsed.message ?? response.statusText)
    }
    throw new ApiError(response.status, parsed.message ?? response.statusText)
  }
  return response.json() as Promise<T>
}

export function postTrackerEvents(
  trackerId: string,
  req: TrackerEventsRequest,
): Promise<TrackerEventsResponse> {
  return requestTrackerText(`/api/tracker/${trackerId}/events`, 'POST', req)
}

/** Per-event structural preview — see `TrackerPreviewResponse`'s doc comment.
 * Never mutates the session; safe to call on every keystroke's committed word
 * (debounced by the caller). */
export function previewTrackerEvents(
  trackerId: string,
  req: TrackerPreviewRequest,
): Promise<TrackerPreviewResponse> {
  return requestTrackerText(`/api/tracker/${trackerId}/preview`, 'POST', req)
}

/** Rebuild the whole session from its initial (pre-first-turn) belief using a
 * corrected/edited FULL script — the only way an edit to an already-committed
 * turn takes effect (see `poke_rust::bin::server::tracker::rebuild_tracker_history`'s
 * doc comment). The response's `log` is the WHOLE log — replace the client's
 * local log with it, don't append. */
export function rebuildTrackerHistory(
  trackerId: string,
  req: TrackerEventsRequest,
): Promise<GetTrackerResponse> {
  return requestTrackerText(`/api/tracker/${trackerId}/history`, 'PUT', req)
}

export function getTrackerCompletions(trackerId: string): Promise<TrackerCompletionsDto> {
  return request(`/api/tracker/${trackerId}/completions`)
}

/** The full teamsheet-legal species list, for the tracker setup form's opponent
 * picker. Session-free on purpose — see `SpeciesListDto`. */
export function listSpecies(): Promise<SpeciesListDto> {
  return request('/api/dex/species')
}

/** Consumes the `GET /api/benchmark` Server-Sent Events stream.
 *
 * Three unbounded sweeps run sequentially server-side and each reports
 * independently: `progress` throughout, then one `result` when it finishes or
 * one `failed` if it does not. Neither closes the stream — a failed sweep must
 * not cancel the two still waiting to run.
 * Only `done`, sent once all three have reported, ends it.
 *
 * `failed` is named that rather than `error` so it cannot be confused with
 * `EventSource`'s own built-in connection-level `error` (a plain `Event`, not a
 * `MessageEvent` with `.data`). That one is a whole-stream failure and is
 * reported through `onAborted`.
 *
 * Closing on `done` matters: `EventSource` auto-reconnects by default, which
 * would silently re-trigger the entire multi-minute run. The returned function
 * cancels early (e.g. on unmount). */
export function streamBenchmark(handlers: {
  onProgress: (progress: BenchmarkProgress) => void
  onResult: (result: BenchmarkResult) => void
  onSweepFailed: (failure: BenchmarkSweepError) => void
  /** Every sweep has reported; the stream is closed. */
  onDone: () => void
  /** The stream itself died, so sweeps still running will never report. */
  onAborted: (message: string) => void
}): () => void {
  const es = new EventSource('/api/benchmark')
  es.addEventListener('progress', (e) => handlers.onProgress(JSON.parse(e.data)))
  es.addEventListener('result', (e) => handlers.onResult(JSON.parse(e.data)))
  es.addEventListener('failed', (e) => handlers.onSweepFailed(JSON.parse(e.data)))
  es.addEventListener('done', () => {
    es.close()
    handlers.onDone()
  })
  es.onerror = () => {
    es.close()
    handlers.onAborted('Connection to server lost')
  }
  return () => es.close()
}
