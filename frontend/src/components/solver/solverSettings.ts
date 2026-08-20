import type { BotAlgorithm, BotProfileRequest } from '../../api/types'

/** The searches that a tracker belief accepts, and what each one reads.
 *
 * The first three read the belief itself, so they respect the fog of war. The
 * last one is exact for its depth, but it reads one drawn opponent, so it
 * answers a perfect-information game.
 *
 * The list lives here rather than in the panel, because the picker sits in the
 * settings sidebar and the panel shows the hint of the selected entry. */
export const SOLVER_ALGORITHMS: { value: BotAlgorithm; label: string; hint: string }[] = [
  {
    value: 'ismcts',
    label: 'ISMCTS (sampled belief)',
    hint: 'Sampled: it draws several possible opponents from the belief, then searches all of them.',
  },
  {
    value: 'mccfr',
    label: 'MCCFR (sampled belief)',
    hint: 'Sampled: it learns a mixed strategy from repeated self-play over the belief.',
  },
  {
    value: 'pimc',
    label: 'PIMC (averaged worlds)',
    hint: 'Baseline: it solves each drawn world exactly and averages the strategies. Each world plays as if the hidden data were known, so the answer claims more than a real player can do.',
  },
  {
    value: 'doubleOracle',
    label: 'Double oracle (exact, one world)',
    hint: 'Exact for its depth, but it reads one drawn opponent. It reports each round while it runs.',
  },
]

/** The default search: a sampled belief search, which respects the fog of war. */
export const DEFAULT_SOLVER_ALGORITHM: BotAlgorithm = 'ismcts'

/** The hint of one search, or an empty string for a name this build removed. */
export function solverAlgorithmHint(algorithm: BotAlgorithm): string {
  return SOLVER_ALGORITHMS.find((option) => option.value === algorithm)?.hint ?? ''
}

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
