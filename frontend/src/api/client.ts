import type {
  BenchmarkProgress,
  BenchmarkResponse,
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
      // non-JSON error body; keep the status text
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
      // non-JSON error body; fall through to the generic message below
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

/** Consumes the `GET /api/benchmark` Server-Sent Events stream — a full,
 * unbounded sweep (matching the offline `cargo bench` binaries), so this can
 * run for several minutes. Named `failed` (not `error`) for the server-side
 * failure event so it can't be confused with `EventSource`'s own built-in
 * connection-level `error` (a plain `Event`, not a `MessageEvent` with
 * `.data`) — that's routed to `onFailed` too, but with a generic message.
 *
 * Callers must let this close the connection on `result`/`failed` (done
 * below) — `EventSource` auto-reconnects by default, which would otherwise
 * silently re-trigger the whole sweep. The returned function lets a caller
 * cancel early too (e.g. on unmount). */
export function streamBenchmark(handlers: {
  onProgress: (progress: BenchmarkProgress) => void
  onResult: (result: BenchmarkResponse) => void
  onFailed: (message: string) => void
}): () => void {
  const es = new EventSource('/api/benchmark')
  es.addEventListener('progress', (e) => handlers.onProgress(JSON.parse(e.data)))
  es.addEventListener('result', (e) => {
    handlers.onResult(JSON.parse(e.data))
    es.close()
  })
  es.addEventListener('failed', (e) => {
    handlers.onFailed(JSON.parse(e.data).message)
    es.close()
  })
  es.onerror = () => {
    handlers.onFailed('Connection to server lost')
    es.close()
  }
  return () => es.close()
}
