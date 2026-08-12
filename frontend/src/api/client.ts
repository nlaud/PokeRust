import type {
  BenchmarkProgress,
  BenchmarkResult,
  BenchmarkSweepError,
  AnalysisProgressResponse,
  CreateBattleRequest,
  CreateBattleResponse,
  CreateTrackerRequest,
  CreateTrackerResponse,
  GetBattleResponse,
  GetTrackerResponse,
  LegalCommands,
  PlayerId,
  SpeciesListDto,
  TrackerAnalysisRequest,
  TrackerAnalysisResponse,
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

/** Tracker parse error with a one-based line number. */
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

export function getAnalysis(battleId: string): Promise<AnalysisProgressResponse> {
  return request(`/api/battles/${battleId}/analysis`)
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

/** Sends tracker text and handles its line-specific parse error format. */
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

/** Gets a structural preview without changing the session. */
export function previewTrackerEvents(
  trackerId: string,
  req: TrackerPreviewRequest,
): Promise<TrackerPreviewResponse> {
  return requestTrackerText(`/api/tracker/${trackerId}/preview`, 'POST', req)
}

/** Rebuilds a tracker session from its initial belief and complete script.
 * Replace the local log with the returned complete log. */
export function rebuildTrackerHistory(
  trackerId: string,
  req: TrackerEventsRequest,
): Promise<GetTrackerResponse> {
  return requestTrackerText(`/api/tracker/${trackerId}/history`, 'PUT', req)
}

export function getTrackerCompletions(trackerId: string): Promise<TrackerCompletionsDto> {
  return request(`/api/tracker/${trackerId}/completions`)
}

/** Stores one solver profile and starts the first depth rung.
 * A second call replaces the profile and restarts the search. */
export function startTrackerAnalysis(
  trackerId: string,
  req: TrackerAnalysisRequest,
): Promise<TrackerAnalysisResponse> {
  return request(`/api/tracker/${trackerId}/analysis`, {
    method: 'POST',
    body: JSON.stringify(req),
  })
}

/** Reads the newest rung and the phase of the search. */
export function getTrackerAnalysis(trackerId: string): Promise<TrackerAnalysisResponse> {
  return request(`/api/tracker/${trackerId}/analysis`)
}

/** Removes the profile and stops the search. */
export function stopTrackerAnalysis(trackerId: string): Promise<void> {
  return request(`/api/tracker/${trackerId}/analysis`, { method: 'DELETE' })
}

/** Gets all teamsheet species for the tracker setup page. */
export function listSpecies(): Promise<SpeciesListDto> {
  return request('/api/dex/species')
}

/** Reads the benchmark event stream.
 * Each test sends progress and then a result or failure.
 * The `done` event closes the stream and prevents automatic reconnection.
 * The returned function cancels the stream. */
export function streamBenchmark(handlers: {
  onProgress: (progress: BenchmarkProgress) => void
  onResult: (result: BenchmarkResult) => void
  onSweepFailed: (failure: BenchmarkSweepError) => void
  /** Called after all tests report and the stream closes. */
  onDone: () => void
  /** Called when a connection failure closes the stream. */
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
