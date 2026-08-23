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
  /** The budget of a search that finishes by itself.
   *
   * Double Oracle, Serialized Bounds, Backward Induction, and PIMC stop when
   * the matrix is solved, so this must hold that solve. */
  simulationTurnBudget: number
  /** The budget of a search that never finishes by itself.
   *
   * ISMCTS, MCCFR, and MCTS spend every turn they are given, so this sets how
   * many seconds one answer takes. It is much smaller for that reason.
   * `PresetLimits` on the server holds the same pair. */
  sampledSimulationTurnBudget: number
  depth: number
  replacementDepth: number | null
  damageRolls: number
  considerCrit: boolean
  particles: number
  /** Reaches the depth by refinement instead of by a complete search. */
  refine: boolean
}

/** The depth that every preset runs.
 *
 * One turn of lookahead resolves the turn and scores each outcome with the leaf
 * evaluator. A second turn costs a whole depth-one solve for each cell of the
 * root matrix, which a doubles position cannot finish. The presets therefore
 * spend their budget on the width of one turn: more damage rolls give a truer
 * outcome distribution, and more worlds give a truer opponent.
 *
 * `PRESET_DEPTH` in `poke_rust/src/bin/server/bot.rs` holds the same value. The
 * Depth field below still accepts a deeper request. */
export const PRESET_DEPTH = 1

/** The largest particle count the server accepts, from `MAX_PARTICLES`. */
export const MAX_PARTICLES = 64

export const DEFAULT_SOLVER_SETTINGS: SolverSettings = {
  simulationTurnBudget: 3_491_884,
  sampledSimulationTurnBudget: 33_600,
  depth: PRESET_DEPTH,
  replacementDepth: null,
  damageRolls: 8,
  considerCrit: false,
  particles: 24,
  refine: false,
}

export type SolverPreset = 'fast' | 'balanced' | 'competitive' | 'custom'

/** The preset table, which mirrors `PresetLimits` on the server.
 *
 * Keep both tables equal. The server resolves a preset name by itself, so a
 * table that drifts shows one set of limits and runs another.
 *
 * The budget here is the one that a search which finishes takes. A sampled
 * search takes a much smaller budget, because it spends every turn it gets and
 * never finishes. `solverProfile` sends no budget for a named preset, so the
 * server picks the right one from the algorithm. */
export const SOLVER_PRESETS: Record<Exclude<SolverPreset, 'custom'>, SolverSettings> = {
  fast: {
    ...DEFAULT_SOLVER_SETTINGS,
    simulationTurnBudget: 418_304,
    sampledSimulationTurnBudget: 27_530,
    damageRolls: 3,
    particles: 12,
  },
  balanced: { ...DEFAULT_SOLVER_SETTINGS },
  competitive: {
    ...DEFAULT_SOLVER_SETTINGS,
    simulationTurnBudget: 5_358_620,
    sampledSimulationTurnBudget: 86_220,
    damageRolls: 16,
    particles: 48,
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
    // A search that finishes needs a budget that holds one whole solve. A
    // sampled search spends every turn it gets, so its budget sets the seconds
    // of one answer. Send the one that matches the algorithm. `PresetLimits`
    // on the server makes the same choice for a request that sends neither.
    simulationTurnBudget: budgetFor(algorithm, settings),
    depth: settings.depth,
    replacementDepth: settings.replacementDepth ?? undefined,
    damageRolls: settings.damageRolls,
    considerCrit: settings.considerCrit,
    particles: isBeliefSearch(algorithm) ? settings.particles : undefined,
    refine: supportsRefinement(algorithm) ? settings.refine : undefined,
  }
}

/** The budget that one search reads from these settings.
 *
 * `PresetLimits::budget_for_algorithm` on the server holds the same rule. */
export function budgetFor(algorithm: BotAlgorithm, settings: SolverSettings): number {
  return finishesByItself(algorithm)
    ? settings.simulationTurnBudget
    : settings.sampledSimulationTurnBudget
}

/** True when this search stops on its own rather than on the budget.
 *
 * An exact search and PIMC solve a matrix and then stop. Every other search
 * walks trajectories until something stops it. */
export function finishesByItself(algorithm: BotAlgorithm): boolean {
  return supportsRefinement(algorithm)
}

/** The searches that can reach their depth by refinement.
 *
 * A refinement pass raises the cells of a matrix equilibrium, so it needs a
 * search that solves a matrix. A sampled search walks trajectories and holds no
 * support to raise. `pimc` qualifies, because each of its worlds is exact.
 *
 * The server holds the same rule in `BotAlgorithm::supports_refinement`, and it
 * rejects the flag for every other name. */
export function supportsRefinement(algorithm: BotAlgorithm): boolean {
  return (
    algorithm === 'doubleOracle' ||
    algorithm === 'backwardInduction' ||
    algorithm === 'serializedBounds' ||
    algorithm === 'pimc'
  )
}

/** True when this search reads a belief rather than the true position. */
export function isBeliefSearch(algorithm: BotAlgorithm): boolean {
  return IMPERFECT_SOLVERS.some((option) => option.value === algorithm)
}
