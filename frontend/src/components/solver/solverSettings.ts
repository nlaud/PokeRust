import type { BotAlgorithm, BotProfileRequest } from '../../api/types'

export interface SolverSettings {
  /** `null` leaves the budget to the server, which derives it from the depth
   * and the particle count. See {@link autoSimulationTurnBudget}. */
  simulationTurnBudget: number | null
  depth: number
  replacementDepth: number | null
  damageRolls: number
  considerCrit: boolean
  particles: number
}

export const DEFAULT_SOLVER_SETTINGS: SolverSettings = {
  simulationTurnBudget: null,
  depth: 2,
  replacementDepth: null,
  damageRolls: 1,
  considerCrit: false,
  particles: 8,
}

const BELIEF: BotAlgorithm[] = ['ismcts', 'mccfr']
const SAMPLED: BotAlgorithm[] = ['mcts', 'ismcts', 'mccfr']

/** The default budget of an exact search, and the floor of a derived budget. */
const FLAT_BUDGET = 1000
/** Rollouts for each particle that a derived budget buys. */
const ROLLOUTS_PER_PARTICLE = 500
/** The ceiling of a derived budget. */
const MAX_DERIVED_BUDGET = 100_000

/** The budget that the server derives for an absent `simulationTurnBudget`.
 *
 * A sampled search spends about one turn simulation for each turn of depth on
 * one rollout, and one rollout reads one particle. Scaling the budget by both
 * numbers holds the rollouts for each particle steady, so a deeper search does
 * not silently become a noisier one.
 *
 * This repeats the rule of `bot.rs::default_simulation_turn_budget`. Keep the
 * two in step. */
export function autoSimulationTurnBudget(
  algorithm: BotAlgorithm,
  settings: SolverSettings,
): number {
  if (!SAMPLED.includes(algorithm)) return FLAT_BUDGET
  const particles = isBeliefSearch(algorithm) ? Math.max(1, settings.particles) : 1
  const derived = ROLLOUTS_PER_PARTICLE * particles * settings.depth
  return Math.min(MAX_DERIVED_BUDGET, Math.max(FLAT_BUDGET, derived))
}

/** Builds a request with only the limits that the selected search reads. */
export function solverProfile(
  algorithm: BotAlgorithm,
  settings: SolverSettings,
): BotProfileRequest {
  return {
    algorithm,
    // An absent field lets the server derive the budget from the other limits.
    simulationTurnBudget: settings.simulationTurnBudget ?? undefined,
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
