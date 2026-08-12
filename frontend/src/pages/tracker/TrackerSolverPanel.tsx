import { useEffect, useState } from 'react'
import Select from '../../components/common/Select'
import type {
  BotAlgorithm,
  BotPreset,
  TrackerAnalysisCheckpoint,
  TrackerStrategyRow,
} from '../../api/types'
import { useTracker } from '../../store/trackerStore'

// The knobs mirror the simulator's `botP2` profile: one algorithm and one
// preset. The preset supplies every limit. The hints describe the tracker,
// where the search reads a belief rather than a resolved battle.

const ALGORITHM_OPTIONS: { value: BotAlgorithm; label: string; hint: string }[] = [
  {
    value: 'doubleOracle',
    label: 'Double Oracle (exact)',
    hint: 'Exact: it solves the drawn world to the depth horizon and returns the true mixed strategy of that horizon.',
  },
  {
    value: 'serializedBounds',
    label: 'Serialized Bounds (exact)',
    hint: 'Exact: the same answer as Double Oracle through alpha-beta bounds.',
  },
  {
    value: 'backwardInduction',
    label: 'Backward Induction (exact)',
    hint: 'Exact: it builds the whole payoff matrix of every turn. The slowest exact algorithm.',
  },
  {
    value: 'mcts',
    label: 'MCTS (sampled)',
    hint: 'Sampled: it plays random lines through the drawn world and keeps the best. The answer is an estimate.',
  },
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
]

const PRESET_OPTIONS: { value: BotPreset; label: string; hint: string }[] = [
  { value: 'fast', label: 'Fast', hint: 'One turn deep, about a second.' },
  { value: 'balanced', label: 'Balanced', hint: 'Two turns deep, about ten seconds.' },
  { value: 'strong', label: 'Strong', hint: 'Three turns deep, up to forty seconds.' },
]

/** The algorithms that enumerate every damage roll of every turn. */
const EXACT_ALGORITHMS: BotAlgorithm[] = ['doubleOracle', 'serializedBounds', 'backwardInduction']

/** Warns that an exact search can run far past its time limit.
 *
 * The solver stops before it starts another turn simulation, but a simulation
 * that already runs still finishes. One multi-hit move at 16 damage rolls
 * branches millions of ways, and that one turn can take minutes. A sampling
 * algorithm draws one outcome, so it does not have this cost. */
const EXACT_TIME_NOTE =
  'An exact algorithm enumerates every damage roll. One multi-hit move, such as Scale Shot, can make one turn take minutes, and the time limit cannot stop a turn that already runs.'

/** How often the panel reads the newest rung while a search runs. */
const POLL_MS = 1000

/** Renders one probability as a whole-percent label. */
function percent(odds: number): string {
  return `${(100 * odds).toFixed(1)}%`
}

/** Renders the change in Player 1's win odds since the last committed turn. */
function deltaLabel(current: number, previous: number): string {
  const change = 100 * (current - previous)
  if (Math.abs(change) < 0.05) return 'no change'
  return `${change > 0 ? '+' : ''}${change.toFixed(1)} pts`
}

/** Renders one joint action as its per-slot descriptions. */
function actionLabel(row: TrackerStrategyRow): string {
  if (row.commands.length === 0) return 'No action'
  return row.commands.map((option) => option.description).join(' · ')
}

/** Shows the strategy of one player, highest rate first. */
function StrategyList({
  title,
  rows,
  testId,
}: {
  title: string
  rows: TrackerStrategyRow[]
  testId: string
}) {
  return (
    <div className="min-w-0 flex-1" data-testid={testId}>
      <p className="mb-1 font-semibold">{title}</p>
      {rows.length === 0 && <p className="text-ink-muted">No action available.</p>}
      {rows.map((row, index) => (
        <div key={index} className="flex items-baseline justify-between gap-2">
          <span className="truncate text-ink-muted">{actionLabel(row)}</span>
          <span className="shrink-0 font-mono">{percent(row.probability)}</span>
        </div>
      ))}
    </div>
  )
}

/** Shows the win odds, the depth, and both strategies of one rung. */
function CheckpointBody({
  checkpoint,
  previousP1WinOdds,
  targetDepth,
}: {
  checkpoint: TrackerAnalysisCheckpoint
  previousP1WinOdds: number | null
  targetDepth: number | null
}) {
  const [showWarnings, setShowWarnings] = useState(false)
  return (
    <div className="mt-2 border-t border-subtle pt-2">
      <div className="flex flex-wrap items-baseline gap-x-3 gap-y-1">
        <span className="rounded-card bg-primary px-2 py-0.5 font-semibold text-white">
          You {percent(checkpoint.p1WinOdds)}
        </span>
        <span className="font-semibold" data-testid="tracker-solver-opponent-odds">
          Opponent {percent(checkpoint.p2WinOdds)}
        </span>
        {/* A stale answer describes the previous position, so its odds are the
            comparison value itself. The change means nothing until a rung of
            the current position arrives. */}
        {previousP1WinOdds !== null && !checkpoint.stale && (
          <span className="text-ink-muted">
            {deltaLabel(checkpoint.p1WinOdds, previousP1WinOdds)} since the last turn
          </span>
        )}
        <span className="ml-auto text-ink-muted" data-testid="tracker-solver-depth">
          Depth {checkpoint.depthReached}
          {targetDepth !== null ? ` of ${targetDepth}` : ''} · turn {checkpoint.turnNumber} ·{' '}
          {(checkpoint.elapsedMs / 1000).toFixed(1)} s
        </span>
      </div>

      {checkpoint.stale && (
        <p className="mt-1 text-warning" data-testid="tracker-solver-stale">
          This answer describes the position before the last committed turn.
        </p>
      )}

      <div className="mt-2 flex flex-col gap-3 sm:flex-row">
        <StrategyList
          title="Your best strategy"
          rows={checkpoint.p1Strategy}
          testId="tracker-solver-p1-strategy"
        />
        <StrategyList
          title={
            checkpoint.p2StrategyIsPlayable
              ? "Opponent's best strategy"
              : 'Opponent action summary'
          }
          rows={checkpoint.p2Strategy}
          testId="tracker-solver-p2-strategy"
        />
      </div>

      {checkpoint.warnings.length > 0 && (
        <div className="mt-2">
          <button
            onClick={() => setShowWarnings(!showWarnings)}
            aria-expanded={showWarnings}
            className="text-ink-muted underline"
          >
            {checkpoint.warnings.length} note(s) on this answer
          </button>
          {showWarnings && (
            <ul className="mt-1 list-disc pl-4 text-ink-muted">
              {checkpoint.warnings.map((line, index) => (
                <li key={index}>{line}</li>
              ))}
            </ul>
          )}
        </div>
      )}
    </div>
  )
}

/**
 * Shows the solver answer for the last committed tracker turn.
 *
 * The tracker holds a belief, so the server draws one world from it and
 * searches that world. The panel shows the win odds and the best strategy of
 * both players, because the tracker user typed both rosters.
 *
 * The server runs one search for each depth, so the numbers move while the
 * search goes deeper. This component reads the newest answer once each second
 * while a search runs.
 */
export default function TrackerSolverPanel() {
  const { analysis, analysisError, startAnalysis, stopAnalysis, refreshAnalysis } = useTracker()
  const [open, setOpen] = useState(false)
  // A sampling belief search is the default. It respects the fog of war, and it
  // answers in a bounded time on every position. See `EXACT_TIME_NOTE`.
  const [algorithm, setAlgorithm] = useState<BotAlgorithm>('ismcts')
  const [preset, setPreset] = useState<BotPreset>('fast')

  const running = analysis?.phase === 'running'
  const on = analysis !== null && analysis.phase !== 'off'
  const checkpoint = analysis?.checkpoint ?? null

  // Restore the controls from the profile that the server stored. A page
  // reload keeps the session, so the controls have to follow it.
  const storedAlgorithm = analysis?.profile?.algorithm
  const storedPreset = analysis?.profile?.preset
  useEffect(() => {
    if (storedAlgorithm) setAlgorithm(storedAlgorithm)
    if (storedPreset) setPreset(storedPreset)
  }, [storedAlgorithm, storedPreset])

  // Read the newest rung while the ladder runs. The timer belongs to this
  // component, so it stops when the user leaves the tracker screen.
  useEffect(() => {
    if (!running) return
    const timer = setInterval(() => {
      void refreshAnalysis()
    }, POLL_MS)
    return () => clearInterval(timer)
  }, [running, refreshAnalysis])

  return (
    <div className="mx-1 mb-1 mt-1 text-[11px]" data-testid="tracker-solver-panel">
      <button
        onClick={() => setOpen(!open)}
        aria-expanded={open}
        data-testid="tracker-solver-toggle"
        className="flex w-full items-center gap-2 rounded-card border border-subtle bg-card px-2 py-1 text-left"
      >
        <span className="font-semibold">Solver</span>
        {checkpoint && (
          <span className="font-mono" data-testid="tracker-solver-summary">
            You {percent(checkpoint.p1WinOdds)} · Opponent {percent(checkpoint.p2WinOdds)}
          </span>
        )}
        {running && <span className="text-ink-muted">searching…</span>}
        {!on && <span className="text-ink-muted">off</span>}
        <span aria-hidden className="ml-auto text-ink-muted">
          {open ? '▾' : '▸'}
        </span>
      </button>

      {open && (
        <div className="mt-1 max-h-72 overflow-y-auto rounded-card border border-subtle bg-card px-2 py-2">
          <div className="flex flex-wrap items-end gap-2">
            <label className="min-w-[11rem] flex-1">
              <span className="mb-0.5 block text-ink-muted">Algorithm</span>
              <Select
                value={algorithm}
                options={ALGORITHM_OPTIONS}
                onChange={(v) => setAlgorithm(v as BotAlgorithm)}
                disabled={running}
              />
            </label>
            <label className="min-w-[9rem] flex-1">
              <span className="mb-0.5 block text-ink-muted">Preset</span>
              <Select
                value={preset}
                options={PRESET_OPTIONS}
                onChange={(v) => setPreset(v as BotPreset)}
                disabled={running}
              />
            </label>
            <button
              onClick={() => void startAnalysis({ algorithm, preset })}
              data-testid="tracker-solver-start"
              className="lift rounded-card bg-primary px-3 py-2 font-semibold text-white"
            >
              {on ? 'Search again' : 'Start search'}
            </button>
            {on && (
              <button
                onClick={() => void stopAnalysis()}
                data-testid="tracker-solver-stop"
                className="lift rounded-card border border-subtle px-3 py-2 font-semibold text-ink-muted"
              >
                Stop
              </button>
            )}
          </div>

          <p className="mt-1 text-ink-muted">
            {ALGORITHM_OPTIONS.find((o) => o.value === algorithm)?.hint}
          </p>
          {EXACT_ALGORITHMS.includes(algorithm) && (
            <p className="mt-1 text-warning" data-testid="tracker-solver-exact-note">
              {EXACT_TIME_NOTE}
            </p>
          )}

          {analysisError && (
            <p className="mt-1 text-danger" data-testid="tracker-solver-request-error">
              {analysisError}
            </p>
          )}
          {analysis?.error && (
            <p className="mt-1 text-danger" data-testid="tracker-solver-error">
              {analysis.error}
            </p>
          )}
          {on && !checkpoint && !analysis?.error && (
            <p className="mt-1 text-ink-muted">
              {running ? 'Searching the last committed turn…' : 'No answer yet.'}
            </p>
          )}

          {checkpoint && (
            <CheckpointBody
              checkpoint={checkpoint}
              previousP1WinOdds={analysis?.previousP1WinOdds ?? null}
              targetDepth={analysis?.targetDepth ?? null}
            />
          )}
        </div>
      )}
    </div>
  )
}
