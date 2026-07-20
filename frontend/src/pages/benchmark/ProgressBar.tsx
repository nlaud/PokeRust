import type { BenchmarkProgress } from '../../api/types'

const STAGE_LABELS: Record<BenchmarkProgress['stage'], string> = {
  turnSpeed: 'Turn speed',
  inference: 'Inference',
}

/** Determinate progress bar driven by real `progress` SSE events from the
 * server (see `api/client.ts::streamBenchmark`) — not a fake timer. Reuses
 * `BenchmarkChart`'s two-rect inline-SVG idiom (track + fill) rather than a
 * charting/UI dependency. The bar resets when `stage` changes (turnSpeed ->
 * inference are two differently-sized sweeps) instead of blending them into
 * one fabricated overall percentage. */
export default function ProgressBar({ progress }: { progress: BenchmarkProgress }) {
  const pct = progress.total > 0 ? Math.min((progress.completed / progress.total) * 100, 100) : 0

  return (
    <div className="mb-6 flex flex-col gap-1.5">
      <div className="flex items-center justify-between text-xs text-ink-muted">
        <span>{STAGE_LABELS[progress.stage]}</span>
        <span className="tabular-nums">
          {progress.completed} / {progress.total}
        </span>
      </div>
      <svg viewBox="0 0 100 10" preserveAspectRatio="none" className="h-3 w-full overflow-visible">
        <rect x={0} y={0} width={100} height={10} rx={2} fill="var(--primary-soft)" />
        <rect x={0} y={0} width={pct} height={10} rx={2} fill="var(--primary)" />
      </svg>
    </div>
  )
}
