import { useState } from 'react'
import Select from '../../components/common/Select'
import SolverControls from '../../components/solver/SolverControls'
import {
  DEFAULT_SOLVER_SETTINGS,
  solverProfile,
} from '../../components/solver/solverSettings'
import type { BotAlgorithm, SolveUpdate, StrategyRow } from '../../api/types'
import { useSolve } from '../../store/solveStore'
import { useTracker } from '../../store/trackerStore'

// These hints describe how each search reads the tracker belief.

const ALGORITHM_OPTIONS: { value: BotAlgorithm; label: string; hint: string }[] = [
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

/** Renders one probability as a whole-percent label. */
function percent(odds: number): string {
  return `${(100 * odds).toFixed(1)}%`
}

/** Renders one change in win odds as a signed point count. */
function points(change: number): string {
  if (Math.abs(change) < 0.05) return 'no change'
  return `${change > 0 ? '+' : ''}${change.toFixed(1)} pts`
}

/** Renders one strategy row.
 *
 * A team-preview row names the leads and the back Pokemon. A battle row names
 * one command for each active slot. */
function actionLabel(row: StrategyRow): string {
  if (row.preview) {
    const leads = row.preview.leads.join(' + ')
    if (row.preview.back.length === 0) return `Lead ${leads}`
    return `Lead ${leads} · back ${row.preview.back.join(' + ')}`
  }
  if (row.commands.length === 0) return 'No action'
  return row.commands.map((option) => option.description).join(' · ')
}

/** The actions that one strategy plays. */
function support(rows: StrategyRow[]): string[] {
  return rows.filter((row) => row.probability > 0).map(actionLabel)
}

/** Which actions entered the support, and which ones left it. */
function supportChange(
  before: StrategyRow[],
  after: StrategyRow[],
): { entered: string[]; left: string[] } {
  const old = new Set(support(before))
  const now = new Set(support(after))
  return {
    entered: [...now].filter((label) => !old.has(label)),
    left: [...old].filter((label) => !now.has(label)),
  }
}

/** Shows notes only when the user points to or focuses the label. */
function NotesTooltip({ label, lines }: { label: string; lines: string[] }) {
  if (lines.length === 0) return null
  return (
    <span className="group relative shrink-0">
      <button type="button" className="text-ink-muted underline" aria-label={`Show ${label}`}>
        {label}
      </button>
      <span className="pointer-events-none absolute bottom-full right-0 z-50 mb-1 hidden w-80 rounded-card border border-subtle bg-card p-2 text-left text-ink-muted shadow-lg group-hover:block group-focus-within:block">
        {lines.map((line, index) => (
          <span key={index} className="mb-1 block last:mb-0">
            {line}
          </span>
        ))}
      </span>
    </span>
  )
}

/** Shows the complete mixed strategy of one player, highest rate first.
 *
 * Every row is text. The panel submits no command, so a suggestion never
 * reaches the input bar on its own. */
function StrategyList({
  title,
  rows,
  testId,
}: {
  title: string
  rows: StrategyRow[] | null
  testId: string
}) {
  return (
    <div className="min-w-0 flex-1" data-testid={testId}>
      <p className="mb-1 font-semibold">{title}</p>
      {rows === null && <p className="text-ink-muted">This profile hides these rows.</p>}
      {rows !== null && rows.length === 0 && <p className="text-ink-muted">No action available.</p>}
      {rows !== null && (
        <div className="max-h-48 overflow-y-auto">
          {rows.map((row, index) => (
            <div key={index} className="flex items-baseline justify-between gap-2">
              <span className="truncate text-ink-muted">{actionLabel(row)}</span>
              <span className="shrink-0 font-mono">{percent(row.probability)}</span>
            </div>
          ))}
        </div>
      )}
    </div>
  )
}

/** Names how much the answer moved between the last two depths.
 *
 * A small change means that another depth is unlikely to change the plan. A
 * large change means the opposite. */
function Stability({ current, previous }: { current: SolveUpdate; previous: SolveUpdate }) {
  const change = supportChange(previous.p1Strategy, current.p1Strategy)
  return (
    <div className="mt-1 text-ink-muted" data-testid="tracker-solver-stability">
      <p>
        Depth {previous.depth} to depth {current.depth}:{' '}
        {points(100 * (current.value - previous.value))}
      </p>
      {change.entered.length === 0 && change.left.length === 0 ? (
        <p>Your support held the same actions.</p>
      ) : (
        <>
          {change.entered.length > 0 && <p>Entered: {change.entered.join(', ')}</p>}
          {change.left.length > 0 && <p>Left: {change.left.join(', ')}</p>}
        </>
      )}
    </div>
  )
}

/** Shows the cost of the search, the seed, and the sampling detail.
 *
 * The seed makes the answer reproducible, so it belongs to every search, not
 * to a sampled search alone. */
function CostLine({ update, seed }: { update: SolveUpdate; seed: number | null }) {
  const { stats, sampling } = update
  const cells =
    stats.matrixCellsTotal > 0
      ? ` · ${stats.matrixCellsEvaluated} of ${stats.matrixCellsTotal} cells`
      : ''
  const sampled = sampling
    ? ` · ${sampling.algorithm}, ${sampling.iterations} iterations${
        sampling.particles === null ? '' : `, ${sampling.particles} worlds`
      } · ${sampling.evaluator} evaluator`
    : ''
  return (
    <p className="mt-1 text-ink-muted" data-testid="tracker-solver-cost">
      {stats.turnsSimulated} turns simulated{cells}
      {sampled}
      {seed === null ? '' : ` · seed ${seed}`}
    </p>
  )
}

/** Shows the win odds, the depth, and both strategies of one answer. */
function AnswerBody({
  answer,
  previous,
  seed,
}: {
  answer: SolveUpdate
  previous: SolveUpdate | null
  seed: number | null
}) {
  const teamPreview = answer.p1Strategy.some((row) => row.preview !== null)
  return (
    <div className="mt-2 border-t border-subtle pt-2">
      <div className="flex flex-wrap items-baseline gap-x-3 gap-y-1">
        <span className="rounded-card bg-primary px-2 py-0.5 font-semibold text-white">
          You {percent(answer.p1WinOdds)}
        </span>
        <span className="font-semibold" data-testid="tracker-solver-opponent-odds">
          Opponent {percent(answer.p2WinOdds)}
        </span>
        <span className="ml-auto text-ink-muted" data-testid="tracker-solver-depth">
          Depth {answer.depth} of {answer.depthTarget} · {(answer.elapsedMs / 1000).toFixed(1)} s ·
          revision {answer.revision}
        </span>
      </div>

      {previous && <Stability current={answer} previous={previous} />}
      <CostLine update={answer} seed={seed} />

      <div className="mt-2 flex flex-col gap-3 sm:flex-row">
        <StrategyList
          title={teamPreview ? 'Your best bring and lead' : 'Your best strategy'}
          rows={answer.p1Strategy}
          testId="tracker-solver-p1-strategy"
        />
        <StrategyList
          title={
            teamPreview
              ? "Opponent's best bring and lead"
              : answer.p2StrategyIsPlayable
                ? "Opponent's best strategy"
                : 'Opponent action summary'
          }
          rows={answer.p2Strategy}
          testId="tracker-solver-p2-strategy"
        />
      </div>

      {answer.warnings.length > 0 && (
        <div className="mt-2">
          <NotesTooltip
            label={`${answer.warnings.length} note(s) on this answer`}
            lines={answer.warnings}
          />
        </div>
      )}
    </div>
  )
}

/**
 * Shows the solver answer for the current tracker position.
 *
 * Before the first `leads` line the position is the team preview, and the
 * answer is one bring-and-lead choice for each player. Every later position is
 * a battle, and the answer is one command for each active slot.
 *
 * The tracker holds a belief, so the server draws worlds from it and searches
 * them. The panel shows the win odds and the complete strategy of both players,
 * because the tracker user typed both rosters.
 *
 * `POST /api/solve` registers one job, and its event stream sends each answer.
 * The panel keeps the last complete answer on screen while the next depth runs.
 *
 * The panel writes no command to the input bar. Every row is text, and the user
 * types the command they choose.
 */
export default function TrackerSolverPanel() {
  const trackerId = useTracker((state) => state.trackerId)
  const { phase, started, progress, complete, previousComplete, live, stale, error, start, stop } =
    useSolve()
  const [open, setOpen] = useState(false)
  // A sampling belief search is the default. It respects the fog of war.
  const [algorithm, setAlgorithm] = useState<BotAlgorithm>('ismcts')
  const [settings, setSettings] = useState(DEFAULT_SOLVER_SETTINGS)

  const running = phase === 'starting' || phase === 'running'
  const on = phase !== 'off'

  return (
    <div
      className="mx-1 mb-1 mt-1 rounded-card border border-subtle bg-card text-[11px]"
      data-testid="tracker-solver-panel"
    >
      <div className="flex items-center gap-2 px-2 py-1">
        <button
          onClick={() => setOpen(!open)}
          aria-expanded={open}
          data-testid="tracker-solver-toggle"
          className="flex min-w-0 flex-1 items-center gap-2 text-left"
        >
          <span className="font-semibold">Solver</span>
          {complete && (
            <span className="font-mono" data-testid="tracker-solver-summary">
              You {percent(complete.p1WinOdds)} · Opponent {percent(complete.p2WinOdds)}
              {stale ? ' · old position' : ''}
            </span>
          )}
          {running && <span className="text-ink-muted">searching…</span>}
          {!on && <span className="text-ink-muted">off</span>}
          <span aria-hidden className="ml-auto text-ink-muted">
            {open ? '▾' : '▸'}
          </span>
        </button>
        <NotesTooltip label="Approx." lines={started?.profile.approximations ?? []} />
      </div>

      {/* The panel grows upward, because the input bar sits at the bottom of
          the arena. The cap is the height of the window, less the room of that
          bar, so the controls and the answer need no scroll. Only an answer
          taller than the window scrolls. */}
      {open && (
        <div className="max-h-[calc(100vh-11rem)] overflow-y-auto border-t border-subtle px-2 py-2">
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
            <button
              onClick={() => {
                if (trackerId === null) return
                void start({
                  source: 'tracker',
                  sessionId: trackerId,
                  profile: solverProfile(algorithm, settings),
                })
              }}
              data-testid="tracker-solver-start"
              className="lift rounded-card bg-primary px-3 py-2 font-semibold text-white"
            >
              {on ? 'Search again' : 'Start search'}
            </button>
            {running && (
              <button
                onClick={() => void stop()}
                data-testid="tracker-solver-stop"
                className="lift rounded-card border border-subtle px-3 py-2 font-semibold text-ink-muted"
              >
                Stop
              </button>
            )}
          </div>

          <SolverControls
            algorithm={algorithm}
            settings={settings}
            disabled={running}
            onChange={setSettings}
          />

          <p className="mt-1 text-ink-muted">
            {ALGORITHM_OPTIONS.find((o) => o.value === algorithm)?.hint}
          </p>
          <p className="mt-1 text-ink-muted" data-testid="tracker-solver-no-submit">
            The panel shows each suggested command as text. It never writes one to the input bar.
          </p>
          {error && (
            <p className="mt-1 text-danger" data-testid="tracker-solver-error">
              {error}
            </p>
          )}
          {on && !complete && !error && (
            <p className="mt-1 text-ink-muted">
              {running ? 'Searching this position…' : 'No answer yet.'}
            </p>
          )}

          {running && live && (
            <p className="mt-1 text-ink-muted" data-testid="tracker-solver-progress">
              Searching depth {live.depth} of {live.depthTarget} · {live.revision + 1} answer(s) so
              far · {live.complete ? 'depth complete' : 'round in progress'}
            </p>
          )}

          {running && progress && (
            <div className="mt-2" data-testid="tracker-solver-turn-progress">
              <div className="mb-1 flex justify-between text-ink-muted">
                <span>Simulation turns</span>
                <span className="font-mono">
                  {progress.turnsSimulated.toLocaleString()} /{' '}
                  {progress.simulationTurnBudget.toLocaleString()}
                </span>
              </div>
              <progress
                className="h-2 w-full"
                value={progress.turnsSimulated}
                max={progress.simulationTurnBudget}
              />
            </div>
          )}

          {complete && stale && (
            <p className="mt-1 text-warning" data-testid="tracker-solver-stale">
              This answer describes the position before the last tracker change.
            </p>
          )}
          {complete && (
            <AnswerBody
              answer={complete}
              previous={previousComplete}
              seed={started?.seed ?? null}
            />
          )}
        </div>
      )}
    </div>
  )
}
