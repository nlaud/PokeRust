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
  /** The depth of a search that enumerates every branch of a turn.
   *
   * Such a search multiplies its tree for each ply, so this stays at
   * `PRESET_DEPTH`. `PresetLimits::depth` on the server holds the same value. */
  depth: number
  /** The depth of a belief search.
   *
   * ISMCTS and MCCFR draw one outcome for each ply, so one more ply costs one
   * more draw and not a whole subtree. Lookahead is what dilutes the error of
   * the leaf evaluator, so a fog-of-war search spends its budget here.
   * `PresetLimits::sampled_depth` on the server holds the same value. */
  sampledDepth: number
  replacementDepth: number | null
  /** The damage rolls of a search that enumerates every branch of a turn.
   *
   * Each roll multiplies the branch set, so this is the most expensive limit of
   * an exact search. `PresetLimits::exact_damage_rolls` holds the same value. */
  damageRolls: number
  /** The damage rolls of a belief search.
   *
   * Such a search draws one outcome for each turn whatever this number is, so a
   * roll widens the set that the draw comes from rather than multiplying the
   * work. Sixteen rolls cost 1.6 times one roll here, against 3,400 times for
   * MCTS. `PresetLimits::sampled_damage_rolls` holds the same value. */
  sampledDamageRolls: number
  considerCrit: boolean
  particles: number
  /** Reaches the depth by refinement instead of by a complete search.
   *
   * The server rejects this flag unless the depth is above the refine base
   * depth of 1, because a request at that depth has nothing to raise. */
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

/** The depth that a belief preset runs.
 *
 * ISMCTS and MCCFR descend one sampled path for each iteration, so one more ply
 * costs one more draw rather than a whole subtree. That makes depth a linear
 * cost for a fog-of-war search and an exponential one for an exact search, so
 * the two families do not share a depth.
 *
 * One turn budget buys about the same seconds at every depth. It buys fewer and
 * deeper iterations instead. A 30-second answer holds about 36,800 iterations at
 * depth 1, 18,200 at depth 2, and 12,100 at depth 3.
 *
 * Depth stops at two. Lookahead dilutes the error of the leaf evaluator, and a
 * doubles root offers about 290 actions, so the visits for each action matter
 * too. No measurement says whether depth 2 or depth 3 plays better.
 *
 * `SAMPLED_PRESET_DEPTH` in `poke_rust/src/bin/server/bot.rs` holds the same
 * value. */
export const SAMPLED_PRESET_DEPTH = 2

/** The largest particle count the server accepts, from `MAX_PARTICLES`. */
export const MAX_PARTICLES = 64

export const DEFAULT_SOLVER_SETTINGS: SolverSettings = {
  simulationTurnBudget: 132_904,
  sampledSimulationTurnBudget: 56_220,
  depth: PRESET_DEPTH,
  sampledDepth: SAMPLED_PRESET_DEPTH,
  replacementDepth: null,
  damageRolls: 2,
  sampledDamageRolls: 16,
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
    simulationTurnBudget: 57_928,
    sampledSimulationTurnBudget: 9_370,
    damageRolls: 1,
    particles: 16,
  },
  balanced: { ...DEFAULT_SOLVER_SETTINGS },
  competitive: {
    ...DEFAULT_SOLVER_SETTINGS,
    simulationTurnBudget: 421_728,
    sampledSimulationTurnBudget: 224_880,
    damageRolls: 3,
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
    // Depth and damage rolls also split by family. An enumerating search
    // multiplies its tree for each ply and for each roll, and a belief search
    // pays one draw for each ply and nothing for a roll.
    depth: depthFor(algorithm, settings),
    replacementDepth: settings.replacementDepth ?? undefined,
    damageRolls: damageRollsFor(algorithm, settings),
    considerCrit: settings.considerCrit,
    particles: isBeliefSearch(algorithm) ? settings.particles : undefined,
    // The server rejects a refine flag that has nothing to raise, so send it
    // only when the depth is above the refine base depth.
    refine:
      supportsRefinement(algorithm) && depthFor(algorithm, settings) > PRESET_DEPTH
        ? settings.refine
        : undefined,
  }
}

/** The depth that one search reads from these settings.
 *
 * `PresetLimits::depth_for_algorithm` on the server holds the same rule. */
export function depthFor(algorithm: BotAlgorithm, settings: SolverSettings): number {
  return enumeratesTurnBranches(algorithm) ? settings.depth : settings.sampledDepth
}

/** The damage rolls that one search reads from these settings.
 *
 * `PresetLimits::damage_rolls_for_algorithm` on the server holds the same
 * rule. */
export function damageRollsFor(algorithm: BotAlgorithm, settings: SolverSettings): number {
  return enumeratesTurnBranches(algorithm) ? settings.damageRolls : settings.sampledDamageRolls
}

/** True when this search builds every branch of a turn before it descends.
 *
 * The exact searches, PIMC, and MCTS enumerate a turn, so each damage roll
 * multiplies their work and each ply multiplies their tree.
 *
 * ISMCTS and MCCFR draw one outcome for each turn instead. A roll only widens
 * the set that the draw comes from, and a ply adds one draw.
 *
 * `BotAlgorithm::enumerates_turn_branches` on the server holds the same rule. */
export function enumeratesTurnBranches(algorithm: BotAlgorithm): boolean {
  return algorithm !== 'ismcts' && algorithm !== 'mccfr'
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
