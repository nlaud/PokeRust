import { useState } from 'react'
import * as api from '../api/client'
import { ApiError } from '../api/client'
import type { BenchmarkResponse, InferenceRow, TurnSpeedRow } from '../api/types'
import { formatTime } from '../lib/time'
import BenchmarkChart, { type ChartRow } from './benchmark/BenchmarkChart'

/** Sorts a scenario's rows for display: enumerate before sample, ascending
 * roll count, no-crit before crit — groups the "enumerate vs sample" story
 * together rather than following the wire order (crit-major). */
function sortRows(rows: TurnSpeedRow[]): TurnSpeedRow[] {
  return [...rows].sort((a, b) => {
    if (a.mode !== b.mode) return a.mode === 'enumerate' ? -1 : 1
    if (a.rolls !== b.rolls) return a.rolls - b.rolls
    return Number(a.crit) - Number(b.crit)
  })
}

function toChartRows(rows: TurnSpeedRow[]): ChartRow[] {
  return sortRows(rows).map((r) => ({
    key: `${r.mode}-${r.rolls}-${r.crit}`,
    label: `${r.mode} · ${r.rolls}r${r.crit ? ' +crit' : ''}`,
    valueSecs: r.avgTimeSecs,
  }))
}

function TurnSpeedSection({ title, rows }: { title: string; rows: TurnSpeedRow[] }) {
  if (rows.length === 0) return null
  return (
    <div className="lift rounded-card bg-card p-4 shadow-sm">
      <h3 className="mb-3 text-sm font-semibold text-ink">{title}</h3>
      <BenchmarkChart rows={toChartRows(rows)} />
      <div className="mt-4 overflow-x-auto">
        <table className="w-full text-left text-xs">
          <thead>
            <tr className="text-ink-muted">
              <th className="pb-1 pr-3 font-medium">Mode</th>
              <th className="pb-1 pr-3 font-medium">Rolls</th>
              <th className="pb-1 pr-3 font-medium">Crit</th>
              <th className="pb-1 pr-3 font-medium">Avg time</th>
              <th className="pb-1 pr-3 font-medium">Avg branches</th>
              <th className="pb-1 font-medium">Pairings</th>
            </tr>
          </thead>
          <tbody>
            {sortRows(rows).map((r) => (
              <tr key={`${r.mode}-${r.rolls}-${r.crit}`} className="border-t border-subtle text-ink">
                <td className="py-1 pr-3">{r.mode}</td>
                <td className="py-1 pr-3 tabular-nums">{r.rolls}</td>
                <td className="py-1 pr-3">{r.crit ? 'yes' : 'no'}</td>
                <td className="py-1 pr-3 tabular-nums">{formatTime(r.avgTimeSecs)}</td>
                <td className="py-1 pr-3 tabular-nums">{r.avgBranches}</td>
                <td className="py-1 tabular-nums">{r.pairings}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </div>
  )
}

function InferenceSection({ rows }: { rows: InferenceRow[] }) {
  if (rows.length === 0) return null
  const chartRows: ChartRow[] = rows.map((r) => ({
    key: r.informationMode,
    label: r.informationMode,
    valueSecs: r.avgTimeSecs,
  }))
  return (
    <div className="lift rounded-card bg-card p-4 shadow-sm">
      <h3 className="mb-3 text-sm font-semibold text-ink">Inference (belief-update) timing</h3>
      <p className="mb-3 text-xs text-ink-muted">
        Average <code>apply_information</code> call time per fog-of-war information mode, replayed across full
        doubles games. A nonzero contradiction count reflects a known, already-tracked inference-engine soundness
        gap (see <code>TODO.md</code>), not a bench defect.
      </p>
      <BenchmarkChart rows={chartRows} />
      <div className="mt-4 overflow-x-auto">
        <table className="w-full text-left text-xs">
          <thead>
            <tr className="text-ink-muted">
              <th className="pb-1 pr-3 font-medium">Mode</th>
              <th className="pb-1 pr-3 font-medium">Calls</th>
              <th className="pb-1 pr-3 font-medium">Avg time</th>
              <th className="pb-1 font-medium">Contradictions</th>
            </tr>
          </thead>
          <tbody>
            {rows.map((r) => (
              <tr key={r.informationMode} className="border-t border-subtle text-ink">
                <td className="py-1 pr-3">{r.informationMode}</td>
                <td className="py-1 pr-3 tabular-nums">{r.calls}</td>
                <td className="py-1 pr-3 tabular-nums">{formatTime(r.avgTimeSecs)}</td>
                <td className={`py-1 tabular-nums ${r.contradictions > 0 ? 'text-warning' : ''}`}>
                  {r.contradictions}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </div>
  )
}

export default function BenchmarkingPage() {
  const [data, setData] = useState<BenchmarkResponse | null>(null)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)

  const run = async () => {
    setBusy(true)
    setError(null)
    try {
      setData(await api.runBenchmark())
    } catch (e) {
      setError(e instanceof ApiError ? e.message : 'Failed to run benchmark')
    } finally {
      setBusy(false)
    }
  }

  const singles = data?.turnSpeed.filter((r) => r.scenario === 'singles') ?? []
  const doubles = data?.turnSpeed.filter((r) => r.scenario === 'doubles') ?? []

  return (
    <div className="mx-auto max-w-4xl p-6">
      <h1 className="mb-2 text-xl font-semibold">Benchmark</h1>
      <p className="mb-6 text-sm text-ink-muted">
        Times one post-team-preview turn across full enumeration vs sample mode, and fog-of-war belief updates
        across full doubles games — a live, bounded version of{' '}
        <code>poke_rust/benches/turn_speed.rs</code> / <code>battle_sweep.rs</code>. Runs a small, capped sweep
        server-side, so it can take up to a minute.
      </p>

      <button
        onClick={run}
        disabled={busy}
        className="lift mb-6 rounded-card bg-primary px-4 py-2 text-sm font-semibold text-white disabled:opacity-40"
      >
        {busy ? 'Running…' : 'Run benchmark'}
      </button>
      {error && <p className="mb-4 text-sm text-danger">{error}</p>}

      {data && (
        <div className="flex flex-col gap-4">
          <TurnSpeedSection title="Turn-resolution speed — singles" rows={singles} />
          <TurnSpeedSection title="Turn-resolution speed — doubles" rows={doubles} />
          <InferenceSection rows={data.inference} />
        </div>
      )}
    </div>
  )
}
