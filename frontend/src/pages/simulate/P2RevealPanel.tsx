import { useEffect, useState } from 'react'
import type { P2Reveal, P2Strategy, StrategyRow } from '../../api/types'
import { useBattle } from '../../store/battleStore'

/** Names the rule that produced the draw. */
const SOURCE_LABELS: Record<P2Reveal['source'], string> = {
  strategy: 'from the solver strategy',
  uniform: 'from a uniform draw',
  teamPreview: 'from a uniform preview draw',
}

/** Explains why a uniform draw replaced the solver strategy.
 *
 * Three rules produce this draw. The search gave no answer for the position.
 * The answer read data that the fog of war hides, which an exact or `mcts`
 * profile does in every fog-of-war battle. The sampled command was not legal
 * in the position.
 *
 * The client waits for the search, so only the first rule also shows a message
 * in the control panel before the turn resolves. */
const UNIFORM_NOTE =
  'Player 2 picked one legal command at random. The search supplied no strategy that Player 2 can play here.'

/** How often the panel reads Player 2's strategy for the current position.
 *
 * The search runs after each turn, so the answer arrives while Player 1 reads
 * the field. Only a battle with `revealStrategy` starts this timer. */
const POLL_MS = 1000

/** Returns the resolved search limits as one short line. */
function replayLine(replay: NonNullable<P2Reveal['replay']>): string {
  const parts = [`depth ${replay.depth}`]
  if (replay.replacementDepth !== null) {
    parts.push(`replacement depth ${replay.replacementDepth}`)
  }
  if (replay.timeMs !== null) {
    parts.push(replay.timeMs < 1000 ? `${replay.timeMs} ms` : `${replay.timeMs / 1000} s`)
  }
  if (replay.nodeBudget !== null) parts.push(`${replay.nodeBudget.toLocaleString()} nodes`)
  if (replay.iterations !== null) parts.push(`${replay.iterations.toLocaleString()} iterations`)
  if (replay.particles !== null) parts.push(`${replay.particles} worlds`)
  if (replay.maxActionsPerPlayer !== null) parts.push(`${replay.maxActionsPerPlayer} actions`)
  parts.push(`${replay.workers} worker`)
  parts.push(`${replay.damageRolls} damage rolls`)
  if (replay.considerCrit) parts.push('crit branches')
  return parts.join(' · ')
}

/** Renders one probability as a whole-percent label. */
function percent(probability: number): string {
  return `${(100 * probability).toFixed(1)}%`
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

/** Shows one full mixed strategy of Player 2, highest rate first. */
function StrategyList({
  title,
  strategy,
  testId,
}: {
  title: string
  strategy: P2Strategy
  testId: string
}) {
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

/**
 * Shows the command that the server drew for Player 2.
 *
 * The panel appears only after both commands lock, so it gives Player 1 no
 * early information. It shows one action for each slot and nothing else of
 * Player 2's plan: no probability of that action, no second action, and no win
 * odds. The wait line replaces the panel while the search runs.
 *
 * A battle whose profile holds `revealStrategy` shows more, because that
 * session asked for it. The panel then also shows the strategy of the current
 * position and the whole strategy that the last drawn command came from. The
 * server sends those rows only for such a session, so this component reads
 * whatever it is given.
 */
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
  } = useBattle()
  const [open, setOpen] = useState(false)

  // Read the newest answer for the current position. The timer belongs to this
  // component, so it stops when Player 1 leaves the battle screen.
  const polls = Boolean(botP2?.revealStrategy) && view !== null && view.phase !== 'gameOver'
  useEffect(() => {
    if (!polls) return
    void refreshP2Strategy()
    const timer = setInterval(() => {
      void refreshP2Strategy()
    }, POLL_MS)
    return () => clearInterval(timer)
  }, [polls, refreshP2Strategy])

  if (!botP2) return null

  if (waitingForBot) {
    // The limit bounds the search, but a turn simulation that already runs
    // cannot stop, so the elapsed time can pass it. The bar therefore stops at
    // a full bar and the seconds keep counting.
    const limit = botP2.timeMs ?? null
    const elapsed = botWaitMs ?? 0
    const percent = limit === null ? null : Math.min(100, Math.round((100 * elapsed) / limit))
    return (
      <div
        data-testid="bot-wait-line"
        role="status"
        className="glass relative z-20 mb-2 rounded-card border border-subtle px-3 py-2 text-xs shadow-sm"
      >
        <div className="flex flex-wrap items-center gap-2">
          <span className="font-semibold">Player 2 is thinking…</span>
          <span className="text-ink-muted">
            The solver is searching this position. The turn resolves when it answers.
          </span>
          <span className="ml-auto shrink-0 font-mono text-ink-muted">
            {(elapsed / 1000).toFixed(1)} s{percent === null ? '' : ` · about ${percent}%`}
          </span>
          <button
            onClick={cancelBotWait}
            data-testid="bot-wait-cancel"
            className="lift shrink-0 rounded-card border border-subtle px-2 py-1 font-semibold text-ink-muted"
          >
            {view?.phase === 'teamPreview' ? 'Change my selection' : 'Change my move'}
          </button>
        </div>
        {percent !== null && (
          <div className="mt-1 h-1 w-full overflow-hidden rounded-card bg-subtle">
            <div
              className="h-full bg-primary"
              style={{ width: `${percent}%` }}
              role="progressbar"
              aria-valuenow={percent}
              aria-valuemin={0}
              aria-valuemax={100}
              aria-label="Player 2 search progress"
            />
          </div>
        )}
      </div>
    )
  }

  const drawnStrategy = p2Reveal?.strategy ?? null
  const played = p2Reveal !== null && p2Reveal.commands.length > 0
  if (!played && !p2Strategy && !drawnStrategy) return null

  return (
    <div
      data-testid="p2-reveal"
      className="glass relative z-20 mb-2 rounded-card border border-subtle px-3 py-2 text-xs shadow-sm"
    >
      <button
        onClick={() => setOpen(!open)}
        aria-expanded={open}
        className="flex w-full min-w-0 flex-wrap items-center gap-2 text-left"
      >
        <span className="rounded-card bg-primary px-2 py-0.5 font-semibold text-white">
          {played ? 'Player 2 played' : 'Player 2 strategy'}
        </span>
        {played && p2Reveal && (
          <>
            <span className="font-semibold">
              {p2Reveal.commands.map((option) => option.description).join(' · ')}
            </span>
            <span className="text-ink-muted">{SOURCE_LABELS[p2Reveal.source]}</span>
          </>
        )}
        {!played && <span className="text-ink-muted">for this position</span>}
        <span aria-hidden className="ml-auto text-ink-muted">
          {open ? '▾' : '▸'}
        </span>
      </button>

      {open && (
        <div className="mt-2 border-t border-subtle pt-2" data-testid="p2-reveal-detail">
          {p2Reveal?.source === 'uniform' && <p className="mb-2 text-warning">{UNIFORM_NOTE}</p>}

          {(p2Strategy || drawnStrategy) && (
            <div className="mb-2 flex flex-col gap-3 sm:flex-row">
              {p2Strategy && (
                <StrategyList
                  title={
                    p2Strategy.position === 'teamPreview'
                      ? "Player 2's bring and lead now"
                      : "Player 2's strategy now"
                  }
                  strategy={p2Strategy}
                  testId="p2-strategy-current"
                />
              )}
              {drawnStrategy && (
                <StrategyList
                  title="The strategy of the last draw"
                  strategy={drawnStrategy}
                  testId="p2-strategy-drawn"
                />
              )}
            </div>
          )}

          {p2Reveal && <p className="text-ink-muted">Draw seed {p2Reveal.drawSeed}</p>}
          {p2Reveal?.replay && (
            <>
              <p className="mt-2 font-semibold">Search</p>
              <p className="text-ink-muted">
                {p2Reveal.replay.algorithm} · {p2Reveal.replay.preset} · turn{' '}
                {p2Reveal.replay.turnNumber} · seed {p2Reveal.replay.searchSeed}
              </p>
              <p className="break-words text-ink-muted">{replayLine(p2Reveal.replay)}</p>
            </>
          )}
        </div>
      )}
    </div>
  )
}
