import type { BotAlgorithm, BotProfileRequest } from '../../api/types'

/** One search, and what it reads.
 *
 * The two lists below split every search by the information that it reads.
 * That split is the same one the server holds: `is_exact` and `Mcts` in
 * `poke_rust/src/bin/server/bot.rs` read the true position, and
 * `searches_belief` names the three that read a belief. */
export interface SolverOption {
  value: BotAlgorithm
  label: string
  hint: string
}

/** The searches that read a belief, so they respect the fog of war.
 *
 * The tracker always uses one of these, because a tracker session holds a
 * belief and never a concrete position. A simulate battle uses one under every
 * information mode except Perfect Information. */
export const IMPERFECT_SOLVERS: SolverOption[] = [
  {
    value: 'ismcts',
    label: 'ISMCTS (sampled belief)',
    hint: 'Sampled: it draws several possible opponents from the belief, then searches all of them. The answer is an estimate, and it respects the fog of war.',
  },
  {
    value: 'mccfr',
    label: 'MCCFR (sampled belief)',
    hint: 'Sampled: it learns a mixed strategy from repeated self-play over the belief. The answer is an estimate, and it respects the fog of war.',
  },
  {
    value: 'pimc',
    label: 'PIMC (averaged worlds)',
    hint: 'Baseline: it solves each drawn world exactly and averages the strategies. Each world plays as if the hidden data were known, so the answer claims more than a real player can do (strategy fusion).',
  },
]

/** The searches that read the true position.
 *
 * A simulate battle uses one of these under Perfect Information. No other
 * information mode can, because the other player holds hidden data that these
 * searches would read. */
export const PERFECT_SOLVERS: SolverOption[] = [
  {
    value: 'doubleOracle',
    label: 'Double Oracle (exact)',
    hint: 'Exact: it solves every turn to the depth horizon and returns the true mixed strategy of that horizon. It reads the true position.',
  },
  {
    value: 'serializedBounds',
    label: 'Serialized Bounds (exact)',
    hint: 'Exact: the same answer as Double Oracle through alpha-beta bounds. It reads the true position.',
  },
  {
    value: 'backwardInduction',
    label: 'Backward Induction (exact)',
    hint: 'Exact: it builds the whole payoff matrix of every turn. The slowest exact algorithm. It reads the true position.',
  },
  {
    value: 'mcts',
    label: 'MCTS (sampled)',
    hint: 'Sampled: it plays random lines and keeps the best. The answer is an estimate. It reads the true position.',
  },
]

export const DEFAULT_IMPERFECT_SOLVER: BotAlgorithm = 'ismcts'
export const DEFAULT_PERFECT_SOLVER: BotAlgorithm = 'doubleOracle'

/** The label of one search, or its raw name for a name this build removed. */
export function solverLabel(algorithm: BotAlgorithm): string {
  const option = [...IMPERFECT_SOLVERS, ...PERFECT_SOLVERS].find((o) => o.value === algorithm)
  return option?.label ?? algorithm
}

/** The hint of one search, or an empty string for a name this build removed. */
export function solverHint(algorithm: BotAlgorithm): string {
  return [...IMPERFECT_SOLVERS, ...PERFECT_SOLVERS].find((o) => o.value === algorithm)?.hint ?? ''
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
    particles: isBeliefSearch(algorithm) ? settings.particles : undefined,
  }
}

/** True when this search reads a belief rather than the true position. */
export function isBeliefSearch(algorithm: BotAlgorithm): boolean {
  return IMPERFECT_SOLVERS.some((option) => option.value === algorithm)
}
