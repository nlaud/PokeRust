import type { BotAlgorithm, BotProfileRequest } from '../../api/types'

export interface SolverSettings {
  simulationTurnBudget: number
  depth: number
  replacementDepth: number | null
  damageRolls: number
  considerCrit: boolean
  particles: number
}

export const DEFAULT_SOLVER_SETTINGS: SolverSettings = {
  simulationTurnBudget: 100_000,
  depth: 3,
  replacementDepth: null,
  damageRolls: 1,
  considerCrit: false,
  particles: 16,
}

export type SolverPreset = 'fast' | 'balanced' | 'competitive' | 'custom'

export const SOLVER_PRESETS: Record<Exclude<SolverPreset, 'custom'>, SolverSettings> = {
  fast: {
    ...DEFAULT_SOLVER_SETTINGS,
    simulationTurnBudget: 10_000,
    depth: 2,
    particles: 8,
  },
  balanced: { ...DEFAULT_SOLVER_SETTINGS },
  competitive: {
    ...DEFAULT_SOLVER_SETTINGS,
    simulationTurnBudget: 500_000,
    depth: 4,
    particles: 32,
  },
}

const BELIEF: BotAlgorithm[] = ['ismcts', 'mccfr', 'pimc']
/** Builds a request with only the limits that the selected search reads. */
export function solverProfile(
  algorithm: BotAlgorithm,
  settings: SolverSettings,
  preset: SolverPreset = 'custom',
): BotProfileRequest {
  return {
    preset,
    algorithm,
    simulationTurnBudget: settings.simulationTurnBudget,
    depth: settings.depth,
    replacementDepth: settings.replacementDepth ?? undefined,
    damageRolls: settings.damageRolls,
    considerCrit: settings.considerCrit,
    particles: BELIEF.includes(algorithm) ? settings.particles : undefined,
  }
}

export function isBeliefSearch(algorithm: BotAlgorithm): boolean {
  return BELIEF.includes(algorithm)
}
