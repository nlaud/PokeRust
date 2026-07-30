import type { BenchmarkProgress } from '../../api/types'

/** Shows progress from real server events.
 * Each benchmark card has its own progress because test sizes differ.
 * `indeterminate` applies before the first progress event. */
export default function ProgressBar({ progress }: { progress: BenchmarkProgress | null }) {
  const indeterminate = progress === null || progress.total === 0
  const pct = indeterminate ? 0 : Math.min((progress.completed / progress.total) * 100, 100)

  return (
    <div className="flex flex-col gap-1.5">
      <div className="flex items-center justify-between text-[11px] text-ink-muted">
        <span>{indeterminate ? 'Starting…' : 'Running…'}</span>
        {!indeterminate && (
          <span className="tabular-nums">
            {progress.completed} / {progress.total}
          </span>
        )}
      </div>
      <svg viewBox="0 0 100 10" preserveAspectRatio="none" className="h-2 w-full overflow-visible">
        <rect x={0} y={0} width={100} height={10} rx={2} fill="var(--primary-soft)" />
        {indeterminate ? (
          <rect
            x={0}
            y={0}
            width={100}
            height={10}
            rx={2}
            fill="var(--primary)"
            className="animate-pulse opacity-30"
          />
        ) : (
          <rect x={0} y={0} width={pct} height={10} rx={2} fill="var(--primary)" />
        )}
      </svg>
    </div>
  )
}
