import { useState } from 'react'
import type { BotProfileView } from '../../api/types'

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

/** Returns the resolved limits as one short line. */
function limitLine(profile: BotProfileView): string {
  const parts = [`depth ${profile.depth}`]
  if (profile.replacementDepth !== null) {
    parts.push(`replacement depth ${profile.replacementDepth}`)
  }
  if (profile.timeMs !== null) {
    parts.push(profile.timeMs < 1000 ? `${profile.timeMs} ms` : `${profile.timeMs / 1000} s`)
  }
  if (profile.nodeBudget !== null) parts.push(`${profile.nodeBudget.toLocaleString()} nodes`)
  if (profile.iterations !== null) parts.push(`${profile.iterations.toLocaleString()} iterations`)
  if (profile.particles !== null) parts.push(`${profile.particles} worlds`)
  if (profile.maxActionsPerPlayer !== null) parts.push(`${profile.maxActionsPerPlayer} actions`)
  parts.push(`${profile.workers} worker`)
  return parts.join(' · ')
}

/**
 * Shows the resolved P2 solver profile.
 * The panel opens to list every approximation and every adjustment.
 */
export default function BotBadge({ profile }: { profile: BotProfileView }) {
  const [open, setOpen] = useState(false)

  return (
    <div
      data-testid="bot-badge"
      className="glass relative z-30 mb-2 rounded-card border border-subtle px-3 py-2 text-xs shadow-sm"
    >
      <button
        onClick={() => setOpen(!open)}
        aria-expanded={open}
        className="flex w-full min-w-0 flex-wrap items-center gap-2 text-left"
      >
        <span className="rounded-card bg-primary px-2 py-0.5 font-semibold text-white">
          P2 solver profile
        </span>
        <span className="font-semibold">{ALGORITHM_LABELS[profile.algorithm]}</span>
        <span className="text-ink-muted">{PRESET_LABELS[profile.preset]}</span>
        <span
          className={`rounded-card px-2 py-0.5 font-semibold ${
            profile.exact ? 'bg-emerald-500/15 text-emerald-700' : 'bg-warning/15 text-warning'
          }`}
        >
          {profile.exact ? 'exact algorithm' : 'sampled algorithm'}
        </span>
        <span aria-hidden className="ml-auto text-ink-muted">
          {open ? '▾' : '▸'}
        </span>
        <span className="basis-full break-words text-ink-muted">{limitLine(profile)}</span>
      </button>

      {open && (
        <div className="mt-2 border-t border-subtle pt-2" data-testid="bot-badge-detail">
          <p className="font-semibold">Approximations</p>
          <ul className="mb-2 list-disc pl-4 text-ink-muted">
            {profile.approximations.map((line) => (
              <li key={line}>{line}</li>
            ))}
          </ul>
          {profile.adjustments.length > 0 && (
            <>
              <p className="font-semibold">Adjustments</p>
              <ul className="list-disc pl-4 text-ink-muted">
                {profile.adjustments.map((line) => (
                  <li key={line}>{line}</li>
                ))}
              </ul>
            </>
          )}
          {profile.seed !== null && <p className="mt-2 text-ink-muted">Seed {profile.seed}</p>}
        </div>
      )}
    </div>
  )
}
