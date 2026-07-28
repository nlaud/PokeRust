import type { BenchmarkProgress } from '../../api/types'
import type { SweepStatus } from '../../store/benchmarkStore'
import BenchmarkChart, {
  BenchmarkChartSkeleton,
  type ChartColumn,
  type ChartRow,
} from './BenchmarkChart'
import ProgressBar from './ProgressBar'

/** One chart in the grid, and the single place a sweep's four states are
 * rendered.
 *
 * The card is what carries loading state, not the page: the three sweeps run
 * concurrently and land minutes apart, so each chart has to show its own
 * progress and fill in independently. A card keeps its footprint across all
 * four states — skeleton rows stand in for the eventual bars — so charts that
 * finish early don't shove later ones around the grid as they arrive. */
export default function ChartCard({
  title,
  subtitle,
  status,
  progress,
  error,
  rows,
  columns,
  reference,
  skeletonRows,
}: {
  title: string
  /** One line under the title saying what the bars encode. */
  subtitle?: string
  status: SweepStatus
  progress: BenchmarkProgress | null
  error: string | null
  rows: ChartRow[]
  columns: ChartColumn[]
  reference?: { value: number; label: string }
  skeletonRows?: number
}) {
  return (
    // `data-status` is what an e2e test should assert on. The four states are
    // otherwise only distinguishable by scraping copy, and these cards are the
    // one place in the app whose state changes on a multi-minute timer.
    <div
      className="lift flex flex-col rounded-card bg-card p-4 shadow-sm"
      data-testid="chart-card"
      data-status={status}
    >
      <div className="mb-3">
        <h3 className="text-sm font-semibold text-ink">{title}</h3>
        {subtitle && <p className="mt-0.5 text-[11px] text-ink-muted">{subtitle}</p>}
      </div>

      {status === 'running' && <ProgressBar progress={progress} />}
      {status === 'running' && (
        <div className="mt-3">
          <BenchmarkChartSkeleton rows={skeletonRows} />
        </div>
      )}

      {status === 'failed' && (
        <p className="text-xs text-danger">{error ?? 'This sweep failed.'}</p>
      )}

      {status === 'done' &&
        (rows.length > 0 ? (
          <>
            <BenchmarkChart rows={rows} columns={columns} reference={reference} />
            {reference && (
              <p className="mt-2 flex items-center gap-1.5 text-[11px] text-ink-muted">
                <span className="inline-block h-px w-4 border-t border-dashed border-current opacity-60" />
                {reference.label}
              </p>
            )}
          </>
        ) : (
          <p className="text-xs text-ink-muted">No rows for this scenario.</p>
        ))}

      {status === 'idle' && <p className="text-xs text-ink-muted">Not run yet.</p>}
    </div>
  )
}
