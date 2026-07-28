import { Fragment } from 'react'

/** A numeric column to the right of the bars. `title` is the explanation of what
 * the column measures — it lives on the header rather than on every cell, so a
 * metric is explained once instead of once per row. */
export interface ChartColumn {
  key: string
  header: string
  title?: string
}

/** One cell of one row. `title` carries row-specific detail the header can't —
 * a caught error message, the exact counts behind a ratio. */
export interface ChartCell {
  text: string
  title?: string
  /** Renders in the warning colour (e.g. a nonzero contradiction count). */
  warn?: boolean
}

export interface ChartRow {
  key: string
  label: string
  /** Explanation of the row's configuration — what "top4" or "BIαβ" mean. */
  labelTitle?: string
  /** The magnitude this row's bar encodes. Which column it corresponds to is
   * the caller's business; the bar is a visual aid for the primary metric. */
  value: number
  /** One entry per column in `columns`, aligned by index. */
  cells: ChartCell[]
  /** Hover text on the bar itself. */
  barTitle?: string
}

/** A horizontal bar per row plus a table of numeric columns.
 *
 * One linear scale per chart, deliberately: callers split e.g. singles from
 * doubles into separate charts rather than sharing a scale, because mixing
 * series that differ by two or three orders of magnitude flattens the smaller
 * one into an invisible sliver. Each chart is a single series, so it needs no
 * legend — its card title names it.
 *
 * Every column shares one CSS grid rather than each row being its own flex row,
 * so columns auto-size to their widest content across all rows and line up
 * everywhere, leaving the bar column identical on every row regardless of how
 * long one row's text happens to be. The template is inline rather than a
 * Tailwind class because the column count is data-driven and Tailwind's JIT
 * only emits classes it can see literally in the source.
 *
 * Hand-rolled inline SVG rather than a charting dependency — the codebase has
 * none today (see `frontend/README.md`). */
export default function BenchmarkChart({
  rows,
  columns,
  reference,
}: {
  rows: ChartRow[]
  columns: ChartColumn[]
  /** Draws a recessive marker at this value on every bar. For a ratio chart the
   * neutral point is the whole story — "is this bar past 1×?" should be
   * answerable by looking, not by reading the number column. */
  reference?: { value: number; label: string }
}) {
  if (rows.length === 0) return null
  const max = Math.max(...rows.map((r) => r.value), 1e-9)
  const referencePct = reference && reference.value <= max ? (reference.value / max) * 100 : null
  const template = `minmax(5rem,10rem) 1fr repeat(${columns.length}, auto)`

  return (
    <div className="overflow-x-auto">
      <div className="grid min-w-fit items-center gap-x-3 gap-y-1.5" style={{ gridTemplateColumns: template }}>
        {/* Header row. `cursor-help` + underline is the only affordance a
            title-attribute tooltip gets, so columns that have an explanation
            have to look different from ones that don't. */}
        <span />
        <span />
        {columns.map((column) => (
          <span
            key={column.key}
            className={`text-right text-[10px] font-medium uppercase tracking-wide text-ink-muted ${
              column.title ? 'cursor-help underline decoration-dotted underline-offset-2' : ''
            }`}
            title={column.title}
          >
            {column.header}
          </span>
        ))}

        {rows.map((row) => {
          // Floored at 1% so a nonzero-but-tiny value still reads as present
          // rather than as an empty track.
          const pct = row.value > 0 ? Math.max((row.value / max) * 100, 1) : 0
          return (
            <Fragment key={row.key}>
              <span
                className={`truncate text-xs text-ink-muted ${
                  row.labelTitle ? 'cursor-help underline decoration-dotted underline-offset-2' : ''
                }`}
                title={row.labelTitle ?? row.label}
              >
                {row.label}
              </span>
              <svg
                viewBox="0 0 100 10"
                preserveAspectRatio="none"
                className="h-3 w-full min-w-24 overflow-visible"
              >
                <title>{row.barTitle ?? row.label}</title>
                <rect x={0} y={0} width={100} height={10} rx={2} fill="var(--border-subtle)" />
                <rect x={0} y={0} width={pct} height={10} rx={2} fill="var(--primary)" />
                {referencePct !== null && (
                  // `non-scaling-stroke` keeps this 1px wide: the viewBox is
                  // stretched horizontally by `preserveAspectRatio="none"`,
                  // which would otherwise smear a vertical line into a band.
                  <line
                    x1={referencePct}
                    x2={referencePct}
                    y1={-1}
                    y2={11}
                    stroke="var(--text-primary)"
                    strokeWidth={1}
                    strokeDasharray="2 2"
                    opacity={0.45}
                    vectorEffect="non-scaling-stroke"
                  />
                )}
              </svg>
              {columns.map((column, i) => {
                const cell = row.cells[i]
                return (
                  <span
                    key={column.key}
                    className={`whitespace-nowrap text-right text-xs tabular-nums ${
                      i === 0 ? 'font-medium text-ink' : 'text-[11px]'
                    } ${cell?.warn ? 'text-warning' : i === 0 ? '' : 'text-ink-muted'} ${
                      cell?.title ? 'cursor-help' : ''
                    }`}
                    title={cell?.title}
                  >
                    {cell?.text ?? '—'}
                  </span>
                )
              })}
            </Fragment>
          )
        })}
      </div>
    </div>
  )
}

/** Placeholder rows shaped like real ones, shown while a sweep is still
 * running. Deliberately uniform-width and unlabelled — a skeleton that varied
 * its bar widths would read as data that had already arrived. */
export function BenchmarkChartSkeleton({ rows = 6 }: { rows?: number }) {
  return (
    <div
      className="grid animate-pulse grid-cols-[minmax(5rem,10rem)_1fr_auto] items-center gap-x-3 gap-y-1.5"
      aria-hidden
    >
      {Array.from({ length: rows }, (_, i) => (
        <Fragment key={i}>
          <span className="h-3 rounded-sm bg-subtle" />
          <span className="h-3 rounded-sm bg-subtle" />
          <span className="h-3 w-12 rounded-sm bg-subtle" />
        </Fragment>
      ))}
    </div>
  )
}
