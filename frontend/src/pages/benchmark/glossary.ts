/** Plain-language explanations of the engine concepts the benchmark charts are
 * labelled with, surfaced as hover tooltips.
 *
 * These are the terms a reader cannot infer from the label — "top4" and "BIαβ"
 * mean nothing without the method behind them. Kept in one file rather than
 * inline in the page so the wording stays consistent between the row labels,
 * the column headers and the card subtitles.
 *
 * Sources of truth for the behaviour described here: `poke_rust::solver`'s
 * module docs (algorithms), `solver::chance::ChanceMode` (sampling),
 * `simulator::{simulate_turn, sample_turn}` (turn-resolution modes) and
 * `information::unknowns::InformationMode` (fog-of-war baselines). Update both
 * if either changes. */

// ── Solver: pruning algorithms ──────────────────────────────────────────────

export const ALGORITHM_LABELS = {
  backwardInduction: 'BI',
  serializedBounds: 'BIαβ',
  doubleOracle: 'DO',
} as const

export const ALGORITHM_HELP = {
  backwardInduction:
    'Backward induction — the unpruned reference. Evaluates every cell of every ' +
    "payoff matrix and solves every matrix game. Each cell is one joint action's " +
    'expected value, so a cell costs a full turn simulation. Correct by ' +
    'construction and the baseline the other two are measured against.',
  serializedBounds:
    'Serialized alpha-beta bounds — before recursing into a subgame, brackets its ' +
    'value by searching two "serialized" versions where one player commits first ' +
    'and the other answers knowing that choice. Moving second can only help you, ' +
    'so those two searches bound the true simultaneous value from above and below; ' +
    'when the bounds meet, the subgame has a pure equilibrium and no matrix is ' +
    'needed at all. It buys skipped matrix cells with extra turn simulations — ' +
    'which is the wrong trade in this engine, and why every one of its bars sits ' +
    'below 1×.',
  doubleOracle:
    'Double oracle — never builds most of the matrix. Starts from a one-by-one ' +
    'restricted game, solves it, then asks each player for their best response ' +
    'over their full action set; if neither can improve, the restricted solution ' +
    'is an equilibrium of the whole game and the remaining cells never mattered. ' +
    'Otherwise it adds those two actions and repeats. Real equilibria have small ' +
    'support, so this typically touches a small fraction of the cells — and a cell ' +
    'is a turn simulation, which is the entire cost of the search.',
} as const

// ── Solver: chance-node policies ────────────────────────────────────────────

export const CHANCE_HELP: Record<string, string> = {
  enumerate:
    'Enumerate — every successor state of a turn, at its true probability. The ' +
    'only exact mode: the value returned is the real game value under the ' +
    'configured damage rolls. Also the most expensive, since the tree multiplies ' +
    'by the successor count at every ply.',
  top4:
    'Top-4 — keeps the four most likely successors and renormalizes them to sum ' +
    'to 1. Captures most of the probability mass at a fraction of the fan-out. ' +
    'Biased toward the middle of the damage distribution, so it understates lines ' +
    'that hinge on an unlikely roll.',
  top1:
    'Top-1 — keeps only the single most likely successor. Cheapest, and the only ' +
    'setting that reaches depth 3 here, but it makes the search effectively blind ' +
    'to damage variance: the rolls that decide whether a borderline attack knocks ' +
    'out are exactly what it discards.',
}

// ── Turn resolution: enumerate vs sample ────────────────────────────────────

export const TURN_MODE_HELP: Record<string, string> = {
  enumerate:
    'Enumerate (`simulate_turn`) — returns every possible outcome of the turn with ' +
    'its probability. Exact, but the branch count multiplies with damage rolls, ' +
    'crit branching, and the number of active Pokemon; doubles spread moves at ' +
    'high roll counts are what make this intractable.',
  sample:
    'Sample (`sample_turn`) — keeps one weighted trajectory at every branch point ' +
    'instead of expanding all of them, returning a single outcome whose ' +
    'probability is that of the sampled path. Cost is bounded regardless of roll ' +
    'count, which is why it barely moves across this chart.',
}

// ── Fog of war: starting information baselines ──────────────────────────────

export const INFORMATION_MODE_HELP: Record<string, string> = {
  closedSheet:
    'Closed team sheet — only the opponent\'s species are visible at team preview, ' +
    'the traditional VGC/Champions competitive format. Moves, item, ability, ' +
    'nature, EVs, IVs and Tera type all stay unknown until revealed through play, ' +
    'so the belief starts at its widest and the engine has the most to narrow.',
  openSheet:
    'Open team sheet — species, ability, item, moves and Tera type are revealed up ' +
    'front, as in a real VGC open sheet. Nature, EVs, IVs and exact stats stay ' +
    'hidden, so inference is doing less work than under a closed sheet.',
  openSheetNatures:
    'Open team sheet plus natures — everything an open sheet reveals, and the ' +
    "Pokemon's nature as well. Only EVs, IVs and exact stats remain to be " +
    'inferred, which is why it is the cheapest of the three.',
}
