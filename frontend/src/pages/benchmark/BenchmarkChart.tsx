import { formatTime } from '../../lib/time'

export interface ChartRow {
  key: string
  label: string
  valueSecs: number
}

/** A horizontal bar per row, scaled against the largest value in `rows` (a
 * linear, per-chart scale — callers split e.g. singles/doubles into separate
 * `BenchmarkChart`s rather than sharing one scale, since mixing scenarios
 * that differ by 2-3 orders of magnitude would flatten the smaller ones to
 * invisible slivers). Hand-rolled inline SVG rather than a charting
 * dependency — the codebase has none today (see `frontend/README.md`). */
export default function BenchmarkChart({ rows }: { rows: ChartRow[] }) {
  if (rows.length === 0) return null
  const max = Math.max(...rows.map((r) => r.valueSecs), 1e-9)

  return (
    <div className="flex flex-col gap-1.5">
      {rows.map((row) => {
        const pct = row.valueSecs > 0 ? Math.max((row.valueSecs / max) * 100, 1) : 0
        return (
          <div key={row.key} className="flex items-center gap-3">
            <span className="w-32 shrink-0 truncate text-xs text-ink-muted" title={row.label}>
              {row.label}
            </span>
            <svg viewBox="0 0 100 10" preserveAspectRatio="none" className="h-3 flex-1 overflow-visible">
              <rect x={0} y={0} width={100} height={10} rx={2} fill="var(--border-subtle)" />
              <rect x={0} y={0} width={pct} height={10} rx={2} fill="var(--primary)" />
            </svg>
            <span className="w-16 shrink-0 text-right text-xs font-medium text-ink tabular-nums">
              {formatTime(row.valueSecs)}
            </span>
          </div>
        )
      })}
    </div>
  )
}
