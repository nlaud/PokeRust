import { Fragment } from 'react'
import Tooltip from '../../components/common/Tooltip'

/** Defines one numeric column beside the bars.
 * `title` explains the metric in the header. */
export interface ChartColumn {
  key: string
  header: string
  title?: string
}

/** Defines one table cell.
 * `title` gives details that apply only to this row. */
export interface ChartCell {
  text: string
  title?: string
  /** Uses the warning color. */
  warn?: boolean
}

export interface ChartRow {
  key: string
  label: string
  /** Explains the row configuration. */
  labelTitle?: string
  /** Sets the bar magnitude for the primary metric. */
  value: number
  /** One entry per column in `columns`, aligned by index. */
  cells: ChartCell[]
  /** Hover text on the bar itself. */
  barTitle?: string
}

/** Shows one horizontal bar and its numeric cells for each row.
 * All rows use one linear scale.
 * One CSS grid aligns all columns.
 * Inline SVG draws the bars without a chart library. */
export default function BenchmarkChart({
  rows,
  columns,
  reference,
}: {
  rows: ChartRow[]
  columns: ChartColumn[]
  /** Draws a reference marker on each bar. */
  reference?: { value: number; label: string }
}) {
  if (rows.length === 0) return null
  const max = Math.max(...rows.map((r) => r.value), 1e-9)
  const referencePct = reference && reference.value <= max ? (reference.value / max) * 100 : null
  const template = `minmax(5rem,10rem) 1fr repeat(${columns.length}, auto)`

  return (
    <div className="overflow-x-auto">
      <div className="grid min-w-fit items-center gap-x-3 gap-y-1.5" style={{ gridTemplateColumns: template }}>
        {/* Dotted underlines mark labels that have a tooltip. */}
        <span />
        <span />
        {columns.map((column) => (
          <Tooltip
            key={column.key}
            content={column.title}
            className={`text-right text-[10px] font-medium uppercase tracking-wide text-ink-muted ${
              column.title ? 'underline decoration-dotted underline-offset-2' : ''
            }`}
          >
            {column.header}
          </Tooltip>
        ))}

        {rows.map((row) => {
          // Show each nonzero value with at least one percent width.
          const pct = row.value > 0 ? Math.max((row.value / max) * 100, 1) : 0
          return (
            <Fragment key={row.key}>
              <Tooltip
                content={row.labelTitle}
                className={`min-w-0 text-xs text-ink-muted ${
                  row.labelTitle ? 'underline decoration-dotted underline-offset-2' : ''
                }`}
              >
                <span className="block truncate">{row.label}</span>
              </Tooltip>
              <svg
                viewBox="0 0 100 10"
                preserveAspectRatio="none"
                className="h-3 w-full min-w-24 overflow-visible"
              >
                <title>{row.barTitle ?? row.label}</title>
                <rect x={0} y={0} width={100} height={10} rx={2} fill="var(--border-subtle)" />
                <rect x={0} y={0} width={pct} height={10} rx={2} fill="var(--primary)" />
                {referencePct !== null && (
                  // Keep this line one pixel wide when the view box stretches.
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
                  <Tooltip
                    key={column.key}
                    content={cell?.title}
                    className={`whitespace-nowrap text-right text-xs tabular-nums ${
                      i === 0 ? 'font-medium text-ink' : 'text-[11px]'
                    } ${cell?.warn ? 'text-warning' : i === 0 ? '' : 'text-ink-muted'} ${
                      cell?.title ? 'underline decoration-dotted underline-offset-2' : ''
                    }`}
                  >
                    {cell?.text ?? '—'}
                  </Tooltip>
                )
              })}
            </Fragment>
          )
        })}
      </div>
    </div>
  )
}

/** Shows uniform placeholder rows while a sweep runs. */
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
