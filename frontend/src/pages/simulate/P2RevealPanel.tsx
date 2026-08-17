import { useEffect, useState } from 'react'
import type { BotProfileView, P2Reveal, P2Strategy, StrategyRow } from '../../api/types'
import { useBattle } from '../../store/battleStore'

const ALGORITHM_LABELS: Record<BotProfileView['algorithm'], string> = {
  doubleOracle: 'Double Oracle',
  serializedBounds: 'Serialized Bounds',
  backwardInduction: 'Backward Induction',
  mcts: 'MCTS',
  ismcts: 'ISMCTS',
  mccfr: 'MCCFR',
}

const PRESET_LABELS: Record<BotProfileView['preset'], string> = {
  fast: 'Fast',
  balanced: 'Balanced',
  strong: 'Strong',
}

const SOURCE_LABELS: Record<P2Reveal['source'], string> = {
  strategy: 'from the solver strategy',
  uniform: 'from a uniform draw',
  teamPreview: 'from a uniform preview draw',
}

const UNIFORM_NOTE =
  'Player 2 picked one legal command at random. The search supplied no strategy that Player 2 can play here.'

const POLL_MS = 1000

function limitLine(profile: BotProfileView): string {
  const parts = [`depth ${profile.depth}`]
  if (profile.replacementDepth !== null) parts.push(`replacement depth ${profile.replacementDepth}`)
  if (profile.timeMs !== null) {
    parts.push(`about ${profile.timeMs < 1000 ? `${profile.timeMs} ms` : `${profile.timeMs / 1000} s`}`)
  }
  if (profile.nodeBudget !== null) parts.push(`${profile.nodeBudget.toLocaleString()} nodes`)
  if (profile.iterations !== null) parts.push(`${profile.iterations.toLocaleString()} iterations`)
  if (profile.particles !== null) parts.push(`${profile.particles} worlds`)
  if (profile.maxActionsPerPlayer !== null) parts.push(`${profile.maxActionsPerPlayer} actions`)
  return parts.join(' · ')
}

function replayLine(replay: NonNullable<P2Reveal['replay']>): string {
  const parts = [`depth ${replay.depth}`]
  if (replay.replacementDepth !== null) parts.push(`replacement depth ${replay.replacementDepth}`)
  if (replay.timeMs !== null) parts.push(`about ${replay.timeMs / 1000} s`)
  if (replay.nodeBudget !== null) parts.push(`${replay.nodeBudget.toLocaleString()} nodes`)
  if (replay.iterations !== null) parts.push(`${replay.iterations.toLocaleString()} iterations`)
  if (replay.particles !== null) parts.push(`${replay.particles} worlds`)
  if (replay.maxActionsPerPlayer !== null) parts.push(`${replay.maxActionsPerPlayer} actions`)
  parts.push(`${replay.damageRolls} damage rolls`)
  if (replay.considerCrit) parts.push('crit branches')
  return parts.join(' · ')
}

function NotesTooltip({ lines }: { lines: string[] }) {
  if (lines.length === 0) return null
  return (
    <span className="group relative shrink-0" data-testid="solver-approximations">
      <button
        type="button"
        aria-label="Show solver approximations"
        className="rounded-card border border-subtle px-2 py-0.5 font-semibold text-ink-muted"
      >
        Approx.
      </button>
      <span className="pointer-events-none absolute right-0 top-full z-50 mt-1 hidden w-80 rounded-card border border-subtle bg-card p-2 text-left text-ink-muted shadow-lg group-hover:block group-focus-within:block">
        {lines.map((line) => (
          <span key={line} className="mb-1 block last:mb-0">
            {line}
          </span>
        ))}
      </span>
    </span>
  )
}

function percent(probability: number): string {
  return `${(100 * probability).toFixed(1)}%`
}

function actionLabel(row: StrategyRow): string {
  if (row.preview) {
    const leads = row.preview.leads.join(' + ')
    if (row.preview.back.length === 0) return `Lead ${leads}`
    return `Lead ${leads} · back ${row.preview.back.join(' + ')}`
  }
  if (row.commands.length === 0) return 'No action'
  return row.commands.map((option) => option.description).join(' · ')
}

function StrategyList({ title, strategy, testId }: { title: string; strategy: P2Strategy; testId: string }) {
  return (
    <div className="min-w-0 flex-1" data-testid={testId}>
      <p className="mb-1 font-semibold">{title}</p>
      {strategy.rows.length === 0 && <p className="text-ink-muted">No action available.</p>}
      {strategy.rows.map((row, index) => {
        const drawn = index === strategy.drawnIndex
        return (
          <div key={index} className="flex items-baseline justify-between gap-2">
            <span className={`truncate ${drawn ? 'font-semibold' : 'text-ink-muted'}`}>
              {drawn ? '▸ ' : ''}
              {actionLabel(row)}
            </span>
            <span className="shrink-0 font-mono">{percent(row.probability)}</span>
          </div>
        )
      })}
    </div>
  )
}

/** Shows one solver card for the profile, progress, result, and reveal. */
export default function P2RevealPanel() {
  const {
    view,
    botP2,
    p2Reveal,
    p2Strategy,
    refreshP2Strategy,
    waitingForBot,
    botWaitMs,
    cancelBotWait,
    finishBotSearch,
  } = useBattle()
  const [open, setOpen] = useState(false)
  const [finishing, setFinishing] = useState(false)

  const polls = Boolean(botP2?.revealStrategy) && view !== null && view.phase !== 'gameOver'
  useEffect(() => {
    if (!polls) return
    void refreshP2Strategy()
    const timer = setInterval(() => void refreshP2Strategy(), POLL_MS)
    return () => clearInterval(timer)
  }, [polls, refreshP2Strategy])

  useEffect(() => {
    if (!waitingForBot) setFinishing(false)
  }, [waitingForBot])

  if (!botP2) return null

  const drawnStrategy = p2Reveal?.strategy ?? null
  const played = p2Reveal !== null && p2Reveal.commands.length > 0
  const elapsed = botWaitMs ?? 0
  const estimate = botP2.timeMs
  const progress = estimate === null ? null : Math.min(99, Math.round((100 * elapsed) / estimate))
  const summary = waitingForBot
    ? 'Player 2 is thinking…'
    : played && p2Reveal
      ? p2Reveal.commands.map((option) => option.description).join(' · ')
      : p2Strategy
        ? 'Strategy ready'
        : 'Preparing strategy'

  return (
    <div
      data-testid="bot-badge"
      className="glass relative z-30 mb-2 rounded-card border border-subtle px-3 py-2 text-xs shadow-sm"
    >
      <div className="flex min-w-0 items-center gap-2">
        <button
          type="button"
          onClick={() => setOpen(!open)}
          aria-expanded={open}
          className="flex min-w-0 flex-1 flex-wrap items-center gap-2 text-left"
        >
          <span className="rounded-card bg-primary px-2 py-0.5 font-semibold text-white">P2 solver</span>
          <span className="font-semibold">{ALGORITHM_LABELS[botP2.algorithm]}</span>
          <span className="text-ink-muted">{PRESET_LABELS[botP2.preset]}</span>
          <span className="truncate text-ink-muted">{summary}</span>
          {botP2.revealStrategy && (
            <span data-testid="bot-badge-reveal" className="rounded-card bg-warning/15 px-2 py-0.5 font-semibold text-warning">
              strategy shown
            </span>
          )}
          <span aria-hidden className="ml-auto text-ink-muted">{open ? '▾' : '▸'}</span>
        </button>
        <NotesTooltip lines={botP2.approximations} />
      </div>

      {waitingForBot && (
        <div data-testid="bot-wait-line" role="status" className="mt-2 border-t border-subtle pt-2">
          <div className="flex flex-wrap items-center gap-2">
            <span className="text-ink-muted">The turn resolves when the solver returns a strategy.</span>
            <span className="ml-auto shrink-0 font-mono text-ink-muted">
              {(elapsed / 1000).toFixed(1)} s{progress === null ? '' : ` · about ${progress}%`}
            </span>
            <button
              type="button"
              onClick={() => {
                setFinishing(true)
                void finishBotSearch()
              }}
              disabled={finishing}
              data-testid="bot-wait-finish"
              className="lift rounded-card bg-primary px-2 py-1 font-semibold text-white disabled:opacity-50"
            >
              {finishing ? 'Choosing…' : 'Choose current move'}
            </button>
            <button
              type="button"
              onClick={cancelBotWait}
              data-testid="bot-wait-cancel"
              className="lift rounded-card border border-subtle px-2 py-1 font-semibold text-ink-muted"
            >
              {view?.phase === 'teamPreview' ? 'Change my selection' : 'Change my move'}
            </button>
          </div>
          {progress !== null && (
            <div className="mt-1 h-1 w-full overflow-hidden rounded-card bg-subtle">
              <div
                className="h-full bg-primary"
                style={{ width: `${progress}%` }}
                role="progressbar"
                aria-valuenow={progress}
                aria-valuemin={0}
                aria-valuemax={100}
                aria-label="Player 2 search progress estimate"
              />
            </div>
          )}
        </div>
      )}

      {open && (
        <div className="mt-2 border-t border-subtle pt-2" data-testid="bot-badge-detail">
          <p className="text-ink-muted">{limitLine(botP2)}</p>
          {botP2.adjustments.length > 0 && (
            <p className="mt-1 text-ink-muted">{botP2.adjustments.join(' · ')}</p>
          )}
          {(played || p2Strategy || drawnStrategy) && (
            <div data-testid="p2-reveal" className="mt-2 border-t border-subtle pt-2">
              {played && p2Reveal && (
                <p className="font-semibold">
                  Player 2 played {p2Reveal.commands.map((option) => option.description).join(' · ')}{' '}
                  <span className="font-normal text-ink-muted">{SOURCE_LABELS[p2Reveal.source]}</span>
                </p>
              )}
              {p2Reveal?.source === 'uniform' && <p className="mt-1 text-warning">{UNIFORM_NOTE}</p>}
              {(p2Strategy || drawnStrategy) && (
                <div className="mt-2 flex flex-col gap-3 sm:flex-row" data-testid="p2-reveal-detail">
                  {p2Strategy && (
                    <StrategyList title="Player 2 strategy now" strategy={p2Strategy} testId="p2-strategy-current" />
                  )}
                  {drawnStrategy && (
                    <StrategyList title="Strategy of the last draw" strategy={drawnStrategy} testId="p2-strategy-drawn" />
                  )}
                </div>
              )}
              {p2Reveal && <p className="mt-2 text-ink-muted">Draw seed {p2Reveal.drawSeed}</p>}
              {p2Reveal?.replay && (
                <p className="mt-1 break-words text-ink-muted">
                  {p2Reveal.replay.algorithm} · {p2Reveal.replay.preset} · turn {p2Reveal.replay.turnNumber} · seed{' '}
                  {p2Reveal.replay.searchSeed} · {replayLine(p2Reveal.replay)}
                </p>
              )}
            </div>
          )}
        </div>
      )}
    </div>
  )
}
