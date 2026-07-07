import type {
  CreateBattleRequest,
  CreateBattleResponse,
  GetBattleResponse,
  LegalCommands,
  PlayerId,
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
