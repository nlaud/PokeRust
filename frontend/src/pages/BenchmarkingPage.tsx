import { useBenchmark } from '../store/benchmarkStore'
import type { InferenceRow, TurnSpeedRow } from '../api/types'
import BenchmarkChart, { type ChartRow } from './benchmark/BenchmarkChart'
import ProgressBar from './benchmark/ProgressBar'

/** Sorts a scenario's rows for display: enumerate before sample, ascending
 * roll count, no-crit before crit — groups the "enumerate vs sample" story
 * together rather than following the wire order (crit-major). */
function sortTurnSpeedRows(rows: TurnSpeedRow[]): TurnSpeedRow[] {
  return [...rows].sort((a, b) => {
    if (a.mode !== b.mode) return a.mode === 'enumerate' ? -1 : 1
    if (a.rolls !== b.rolls) return a.rolls - b.rolls
    return Number(a.crit) - Number(b.crit)
  })
}

function turnSpeedChartRows(rows: TurnSpeedRow[]): ChartRow[] {
  return sortTurnSpeedRows(rows).map((r) => ({
    key: `${r.mode}-${r.rolls}-${r.crit}`,
    label: `${r.mode} · ${r.rolls} ${r.rolls === 1 ? 'roll' : 'rolls'}${r.crit ? ' +crit' : ''}`,
    valueSecs: r.avgTimeSecs,
    annotation: `${r.avgBranches} ${r.avgBranches === 1 ? 'branch' : 'branches'}`,
    annotationTitle: 'Average number of possible outcome states this cell branched into per resolved turn',
  }))
}

function inferenceChartRows(rows: InferenceRow[]): ChartRow[] {
  return rows.map((r) => ({
    key: r.informationMode,
    label: r.informationMode,
    valueSecs: r.avgTimeSecs,
    annotation: `${r.contradictions} / ${r.calls} failed`,
    annotationWarn: r.contradictions > 0,
    annotationTitle:
      r.contradictions > 0
        ? `First caught failure (a known, already-tracked apply_information contradiction — see TODO.md):\n${r.contradictionSample}`
        : undefined,
  }))
}

function ChartCard({ title, rows }: { title: string; rows: ChartRow[] }) {
  if (rows.length === 0) return null
  return (
    <div className="lift rounded-card bg-card p-4 shadow-sm">
      <h3 className="mb-3 text-sm font-semibold text-ink">{title}</h3>
      <BenchmarkChart rows={rows} />
    </div>
  )
}

export default function BenchmarkingPage() {
  const { data, busy, progress, error, run } = useBenchmark()

  const turnSpeedSingles = data?.turnSpeed.filter((r) => r.scenario === 'singles') ?? []
  const turnSpeedDoubles = data?.turnSpeed.filter((r) => r.scenario === 'doubles') ?? []
  const inferenceSingles = data?.inference.filter((r) => r.scenario === 'singles') ?? []
  const inferenceDoubles = data?.inference.filter((r) => r.scenario === 'doubles') ?? []

  return (
    <div className="mx-auto max-w-4xl p-6">
      <h1 className="mb-6 text-xl font-semibold">Benchmark</h1>

      <button
        onClick={run}
        disabled={busy}
        className="lift mb-6 rounded-card bg-primary px-4 py-2 text-sm font-semibold text-white disabled:opacity-40"
      >
        {busy ? 'Running…' : 'Run benchmark'}
      </button>
      {busy && progress && <ProgressBar progress={progress} />}
      {error && <p className="mb-4 text-sm text-danger">{error}</p>}

      {data && (
        <div className="flex flex-col gap-4">
          <ChartCard title="Turn Speed: Singles" rows={turnSpeedChartRows(turnSpeedSingles)} />
          <ChartCard title="Turn Speed: Doubles" rows={turnSpeedChartRows(turnSpeedDoubles)} />
          <ChartCard title="Inference: Singles" rows={inferenceChartRows(inferenceSingles)} />
          <ChartCard title="Inference: Doubles" rows={inferenceChartRows(inferenceDoubles)} />
        </div>
      )}
    </div>
  )
}
