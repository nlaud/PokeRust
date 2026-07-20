import { Fragment } from 'react'
import { formatTime } from '../../lib/time'

export interface ChartRow {
  key: string
  label: string
  valueSecs: number
  /** Compact secondary text after the time label — e.g. a branch count or a
   * contradiction count. There's no table anymore for this to live in, so
   * it rides along on the chart row instead of being dropped. */
  annotation?: string
  /** Styles the annotation as a warning (e.g. a nonzero contradiction count
   * — a known, already-tracked inference-engine signal worth calling out). */
  annotationWarn?: boolean
  /** Hover text on the annotation — e.g. what the branch count means, or the
   * actual caught error message for a nonzero contradiction count. */
  annotationTitle?: string
}

/** A horizontal bar per row, scaled against the largest value in `rows` (a
 * linear, per-chart scale — callers split e.g. singles/doubles into separate
 * `BenchmarkChart`s rather than sharing one scale, since mixing scenarios
 * that differ by 2-3 orders of magnitude would flatten the smaller ones to
 * invisible slivers).
 *
 * All rows share one CSS grid rather than each being its own flex row: the
 * label/time/annotation columns auto-size to their widest content across
 * every row, so those columns land on the same width everywhere and the bar
 * column — whatever's left — is identical on every row, regardless of how
 * long one particular row's time or annotation text happens to be.
 *
 * Hand-rolled inline SVG rather than a charting dependency — the codebase
 * has none today (see `frontend/README.md`). */
export default function BenchmarkChart({ rows }: { rows: ChartRow[] }) {
  if (rows.length === 0) return null
  const max = Math.max(...rows.map((r) => r.valueSecs), 1e-9)

  return (
    <div className="grid grid-cols-[10rem_1fr_auto_auto] items-center gap-x-3 gap-y-1.5">
      {rows.map((row) => {
        const pct = row.valueSecs > 0 ? Math.max((row.valueSecs / max) * 100, 1) : 0
        return (
          <Fragment key={row.key}>
            <span className="truncate text-xs text-ink-muted" title={row.label}>
              {row.label}
            </span>
            <svg viewBox="0 0 100 10" preserveAspectRatio="none" className="h-3 w-full overflow-visible">
              <rect x={0} y={0} width={100} height={10} rx={2} fill="var(--border-subtle)" />
              <rect x={0} y={0} width={pct} height={10} rx={2} fill="var(--primary)" />
            </svg>
            <span className="text-right text-xs font-medium text-ink tabular-nums">{formatTime(row.valueSecs)}</span>
            {row.annotation ? (
              <span
                className={`whitespace-nowrap text-right text-[11px] tabular-nums ${
                  row.annotationWarn ? 'text-warning' : 'text-ink-muted'
                }`}
                title={row.annotationTitle}
              >
                {row.annotation}
              </span>
            ) : (
              <span />
            )}
          </Fragment>
        )
      })}
    </div>
  )
}
