import { useBenchmark } from '../store/benchmarkStore'
import type { InferenceRow, SolverRow, TurnSpeedRow } from '../api/types'
import type { ChartColumn, ChartRow } from './benchmark/BenchmarkChart'
import ChartCard from './benchmark/ChartCard'
import {
  ALGORITHM_HELP,
  ALGORITHM_LABELS,
  CHANCE_HELP,
  INFORMATION_MODE_HELP,
  TURN_MODE_HELP,
} from './benchmark/glossary'
import { formatTime } from '../lib/time'

/** Formats counts like the offline solver benchmark. */
function formatCount(value: number): string {
  if (value >= 1e6) return `${(value / 1e6).toFixed(1)}M`
  if (value >= 1e3) return `${(value / 1e3).toFixed(1)}k`
  return value.toFixed(0)
}

const PAIRINGS_HELP =
  'How many ordered teamsheet pairings from `teamsheets/` this cell was averaged ' +
  'over. Expensive cells get fewer, so a row averaged over one pairing is a single ' +
  'matchup rather than a stable mean.'

// ── Turn speed ──────────────────────────────────────────────────────────────

const TURN_SPEED_COLUMNS: ChartColumn[] = [
  { key: 'time', header: 'time', title: 'Average wall-clock to resolve one turn.' },
  {
    key: 'branches',
    header: 'branches',
    title:
      'Weighted outcome states the turn resolved into. This is what drives the ' +
      'cost: enumeration clones a full BattleState per branch, so time tracks ' +
      'branch count almost linearly. Sample mode is always 1 by construction.',
  },
  { key: 'pairings', header: 'pairs', title: PAIRINGS_HELP },
]

/** Sorts mode, roll count, and critical-hit setting for chart comparison. */
function turnSpeedChartRows(rows: TurnSpeedRow[]): ChartRow[] {
  return [...rows]
    .sort((a, b) => {
      if (a.mode !== b.mode) return a.mode === 'enumerate' ? -1 : 1
      if (a.rolls !== b.rolls) return a.rolls - b.rolls
      return Number(a.crit) - Number(b.crit)
    })
    .map((r) => ({
      key: `${r.mode}-${r.rolls}-${r.crit}`,
      label: `${r.mode} · ${r.rolls} ${r.rolls === 1 ? 'roll' : 'rolls'}${r.crit ? ' +crit' : ''}`,
      labelTitle: `${TURN_MODE_HELP[r.mode]}\n\nDamage rolls: ${r.rolls}. Crit branching: ${
        r.crit ? 'on' : 'off'
      }.`,
      value: r.avgTimeSecs,
      barTitle: `${formatTime(r.avgTimeSecs)} per turn, ${r.avgBranches} outcome states`,
      cells: [
        { text: formatTime(r.avgTimeSecs) },
        { text: `${r.avgBranches}` },
        { text: `${r.pairings}` },
      ],
    }))
}

// ── Inference ───────────────────────────────────────────────────────────────

const INFERENCE_COLUMNS: ChartColumn[] = [
  {
    key: 'time',
    header: 'time',
    title: 'Average wall-clock for one `apply_information` call — folding one turn of observed events into the fog-of-war belief.',
  },
  {
    key: 'calls',
    header: 'calls',
    title: 'How many belief updates were timed, across full games played to completion.',
  },
  {
    key: 'contradictions',
    header: 'failed',
    title:
      'Calls that raised a contradiction — the engine deducing something ' +
      'impossible from a sound observation. These are known, already-tracked ' +
      'inference bugs (see TODO.md); hover a nonzero count for the caught message.',
  },
]

function inferenceChartRows(rows: InferenceRow[]): ChartRow[] {
  return rows.map((r) => ({
    key: r.informationMode,
    label: r.informationMode,
    labelTitle: INFORMATION_MODE_HELP[r.informationMode] ?? r.informationMode,
    value: r.avgTimeSecs,
    barTitle: `${formatTime(r.avgTimeSecs)} per belief update, over ${r.calls} calls`,
    cells: [
      { text: formatTime(r.avgTimeSecs) },
      { text: `${r.calls}` },
      {
        text: `${r.contradictions}`,
        warn: r.contradictions > 0,
        title:
          r.contradictions > 0
            ? `First caught failure (a known, already-tracked apply_information contradiction — see TODO.md):\n${r.contradictionSample}`
            : undefined,
      },
    ],
  }))
}

// ── Solver ──────────────────────────────────────────────────────────────────

const SOLVER_COLUMNS: ChartColumn[] = [
  {
    key: 'turns',
    header: 'turns',
    title:
      'Turn simulations per solve — the cost that matters. A `simulate_turn` call ' +
      'runs in hundreds of microseconds while solving a matrix game takes single ' +
      'digits, so a configuration\'s wall-clock is very nearly this number times a ' +
      'constant. Unlike time, it is unaffected by whatever else the machine is doing.',
  },
  {
    key: 'cells',
    header: 'cells',
    title:
      'Share of the payoff matrix that was actually evaluated; the rest was pruned ' +
      'away. Backward induction is 100% by definition, so anything below that is ' +
      'exactly what double oracle bought.',
  },
  { key: 'time', header: 'time', title: 'Average wall-clock for one solve.' },
  {
    key: 'nodes',
    header: 'nodes',
    title:
      'Decision nodes whose payoff matrix was built. Each is one position where ' +
      'both players choose simultaneously — including mid-turn ones like ' +
      'replacements after a faint, which are real decisions but not new turns.',
  },
  { key: 'pairings', header: 'pairs', title: PAIRINGS_HELP },
]

function ran(row: SolverRow): boolean {
  return !row.skipped && row.pairings > 0
}

function chanceHelp(chance: string): string {
  return CHANCE_HELP[chance] ?? chance
}

/** Shows double-oracle turn counts by depth and chance mode. */
function solverCostRows(rows: SolverRow[], scenario: SolverRow['scenario']): ChartRow[] {
  return rows
    .filter((r) => ran(r) && r.scenario === scenario && r.algorithm === 'doubleOracle')
    .sort((a, b) => a.depth - b.depth || a.rolls - b.rolls || a.chance.localeCompare(b.chance))
    .map((r) => ({
      key: `${r.depth}-${r.rolls}-${r.chance}`,
      // Use `d2` because the full depth label does not fit this card.
      label: `d${r.depth} · ${r.rolls} ${r.rolls === 1 ? 'roll' : 'rolls'} · ${r.chance}`,
      labelTitle:
        `Search depth ${r.depth} — turns of lookahead before positions are scored ` +
        `by the heuristic. ${r.rolls} damage roll${r.rolls === 1 ? '' : 's'} per attack.` +
        `\n\n${chanceHelp(r.chance)}`,
      value: r.avgTurnsSimulated,
      barTitle: `${formatCount(r.avgTurnsSimulated)} turn simulations per solve`,
      cells: [
        { text: formatCount(r.avgTurnsSimulated) },
        {
          text: `${(100 * (r.avgCellsEvaluated / Math.max(r.avgCellsTotal, 1))).toFixed(0)}%`,
          title: `${formatCount(r.avgCellsEvaluated)} of ${formatCount(r.avgCellsTotal)} matrix cells evaluated`,
        },
        { text: formatTime(r.avgTimeSecs) },
        { text: formatCount(r.avgNodes) },
        {
          text: `${r.pairings}`,
          title: r.actionCap
            ? `Joint actions capped at ${r.actionCap} per player, so this is a cost measurement rather than a quality one.`
            : undefined,
        },
      ],
    }))
}

const PRUNING_COLUMNS: ChartColumn[] = [
  {
    key: 'speedup',
    header: 'vs BI',
    title:
      'Turn simulations backward induction needed, divided by what this algorithm ' +
      'needed, at identical settings. Above 1× the pruning saved more than it ' +
      'cost; below 1× it cost more than it saved.',
  },
  {
    key: 'turns',
    header: 'BI → this',
    title: 'The raw turn-simulation counts behind the ratio.',
  },
  {
    key: 'time',
    header: 'time',
    title: 'Wall-clock for one solve with this algorithm, for comparison with the ratio.',
  },
]

/** Compares each pruning algorithm with backward induction.
 * Each ratio uses matched settings and turn counts.
 * The chart sorts ratios from highest to lowest. */
function solverPruningRows(rows: SolverRow[]): ChartRow[] {
  const settings = (r: SolverRow) => `${r.scenario}-${r.depth}-${r.rolls}-${r.chance}`
  const baselines = new Map(
    rows.filter((r) => ran(r) && r.algorithm === 'backwardInduction').map((r) => [settings(r), r]),
  )

  return rows
    .filter((r) => ran(r) && r.algorithm !== 'backwardInduction' && baselines.has(settings(r)))
    .map((candidate) => {
      const baseline = baselines.get(settings(candidate))!
      const algorithm = ALGORITHM_LABELS[candidate.algorithm]
      const speedup = baseline.avgTurnsSimulated / Math.max(candidate.avgTurnsSimulated, 1e-9)
      const scenario = candidate.scenario === 'singles' ? 'S' : 'D'
      return {
        key: `${candidate.algorithm}-${settings(candidate)}`,
        label: `${algorithm} · ${scenario} d${candidate.depth} · ${candidate.chance}`,
        labelTitle:
          `${ALGORITHM_HELP[candidate.algorithm]}\n\n` +
          `${candidate.scenario}, depth ${candidate.depth}, ${candidate.rolls} damage roll` +
          `${candidate.rolls === 1 ? '' : 's'}.\n\n${chanceHelp(candidate.chance)}`,
        value: speedup,
        barTitle:
          `${algorithm} needs ${speedup.toFixed(2)}× ${speedup >= 1 ? 'fewer' : 'MORE'} ` +
          'turn simulations than backward induction',
        cells: [
          { text: `${speedup.toFixed(2)}×` },
          {
            text: `${formatCount(baseline.avgTurnsSimulated)} → ${formatCount(candidate.avgTurnsSimulated)}`,
          },
          {
            text: formatTime(candidate.avgTimeSecs),
            title: `Backward induction took ${formatTime(baseline.avgTimeSecs)} at the same settings.`,
          },
        ],
      }
    })
    .sort((a, b) => b.value - a.value)
}

// ── Page ────────────────────────────────────────────────────────────────────

export default function BenchmarkingPage() {
  const { turnSpeed, inference, solver, busy, streamError, run } = useBenchmark()
  const started = turnSpeed.status !== 'idle'

  return (
    <div className="mx-auto max-w-6xl p-6">
      <h1 className="mb-5 text-xl font-semibold">Benchmark</h1>

      <button
        onClick={run}
        disabled={busy}
        className="lift mb-6 rounded-card bg-primary px-4 py-2 text-sm font-semibold text-white disabled:opacity-40"
      >
        {busy ? 'Running…' : started ? 'Run again' : 'Run benchmark'}
      </button>

      {streamError && <p className="mb-4 text-sm text-danger">{streamError}</p>}

      {started && (
        <div className="grid grid-cols-1 gap-4 lg:grid-cols-2">
          <ChartCard
            title="Turn speed · Singles"
            subtitle="One resolved turn, enumerate vs sample"
            status={turnSpeed.status}
            progress={turnSpeed.progress}
            error={turnSpeed.error}
            columns={TURN_SPEED_COLUMNS}
            rows={turnSpeedChartRows(turnSpeed.rows.filter((r) => r.scenario === 'singles'))}
            skeletonRows={10}
          />
          <ChartCard
            title="Turn speed · Doubles"
            subtitle="One resolved turn, enumerate vs sample"
            status={turnSpeed.status}
            progress={turnSpeed.progress}
            error={turnSpeed.error}
            columns={TURN_SPEED_COLUMNS}
            rows={turnSpeedChartRows(turnSpeed.rows.filter((r) => r.scenario === 'doubles'))}
            skeletonRows={10}
          />

          <ChartCard
            title="Inference · Singles"
            subtitle="One fog-of-war belief update, by starting information"
            status={inference.status}
            progress={inference.progress}
            error={inference.error}
            columns={INFERENCE_COLUMNS}
            rows={inferenceChartRows(inference.rows.filter((r) => r.scenario === 'singles'))}
            skeletonRows={3}
          />
          <ChartCard
            title="Inference · Doubles"
            subtitle="One fog-of-war belief update, by starting information"
            status={inference.status}
            progress={inference.progress}
            error={inference.error}
            columns={INFERENCE_COLUMNS}
            rows={inferenceChartRows(inference.rows.filter((r) => r.scenario === 'doubles'))}
            skeletonRows={3}
          />

          <ChartCard
            title="Solver · Singles"
            subtitle="Cost of one solve, double oracle"
            status={solver.status}
            progress={solver.progress}
            error={solver.error}
            columns={SOLVER_COLUMNS}
            rows={solverCostRows(solver.rows, 'singles')}
            skeletonRows={5}
          />
          <ChartCard
            title="Solver · Doubles"
            subtitle="Cost of one solve, double oracle"
            status={solver.status}
            progress={solver.progress}
            error={solver.error}
            columns={SOLVER_COLUMNS}
            rows={solverCostRows(solver.rows, 'doubles')}
            skeletonRows={4}
          />

          <div className="lg:col-span-2">
            <ChartCard
              title="Solver · Pruning payoff"
              subtitle="Turn simulations each pruning algorithm needs, relative to unpruned backward induction. Above 1× is a win; below 1× costs more than it saves."
              status={solver.status}
              progress={solver.progress}
              error={solver.error}
              columns={PRUNING_COLUMNS}
              rows={solverPruningRows(solver.rows)}
              reference={{ value: 1, label: '1× — same cost as backward induction' }}
              skeletonRows={6}
            />
          </div>
        </div>
      )}
    </div>
  )
}
