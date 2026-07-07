import { useState } from 'react'
import type { PlayerId, PokemonView } from '../../api/types'
import Sprite from '../../components/common/Sprite'
import { useBattle } from '../../store/battleStore'

const STAT_NAMES = ['HP', 'Atk', 'Def', 'SpA', 'SpD', 'Spe']
const BOOST_NAMES = ['Atk', 'Def', 'SpA', 'SpD', 'Spe', 'Acc', 'Eva']

const STATUS_COLORS: Record<string, string> = {
  BRN: 'bg-orange-500',
  PSN: 'bg-purple-500',
  TOX: 'bg-purple-700',
  PAR: 'bg-yellow-500',
  SLP: 'bg-slate-400',
  FRZ: 'bg-cyan-400',
}

function hpBarColor(fraction: number): string {
  if (fraction <= 0.2) return 'bg-danger'
  if (fraction <= 0.5) return 'bg-warning'
  return 'bg-success'
}

function MonRow({ mon, active }: { mon: PokemonView; active: boolean }) {
  const fraction = mon.hp.max > 0 ? mon.hp.current / mon.hp.max : 0

  return (
    <details className={`rounded-card ${active ? 'bg-primary-soft/40' : ''} ${mon.fainted ? 'opacity-50' : ''}`}>
      <summary className="flex cursor-pointer list-none items-center gap-2 p-2 [&::-webkit-details-marker]:hidden">
        <Sprite species={mon.species} size={40} />
        <div className="min-w-0 flex-1">
          <div className="flex items-center justify-between">
            <span className="truncate text-xs font-semibold">
              {mon.species}
              {mon.isTera && <span className="ml-1 text-primary">✦{mon.teraType}</span>}
            </span>
            <span className="text-[10px] text-ink-muted">Lv{mon.level}</span>
          </div>
          <div className="mt-1 h-1.5 overflow-hidden rounded-full bg-subtle">
            <div
              className={`h-full rounded-full transition-all duration-500 ${hpBarColor(fraction)}`}
              style={{ width: `${Math.max(0, fraction) * 100}%` }}
            />
          </div>
          <div className="mt-0.5 flex items-center justify-between">
            <span className="text-[10px] text-ink-muted">
              {mon.hp.current}/{mon.hp.max}
            </span>
            {mon.status && (
              <span
                className={`rounded px-1 text-[9px] font-bold text-white ${
                  STATUS_COLORS[mon.status.code] ?? 'bg-slate-500'
                }`}
              >
                {mon.status.code}
              </span>
            )}
          </div>
        </div>
      </summary>

      <div className="space-y-1.5 px-2 pb-2 text-[11px]">
        <div className="flex flex-wrap gap-x-3 gap-y-0.5 text-ink-muted">
          <span>
            <span className="font-medium text-ink">Item:</span> {mon.item ?? '—'}
          </span>
          <span>
            <span className="font-medium text-ink">Ability:</span> {mon.ability}
          </span>
        </div>

        <div>
          {mon.moves.map((move, i) =>
            move ? (
              <div key={i} className="flex justify-between">
                <span>{move.name}</span>
                <span className="text-ink-muted">
                  {move.pp}/{move.maxPp}
                </span>
              </div>
            ) : null,
          )}
        </div>

        <div className="flex flex-wrap gap-x-2 text-ink-muted">
          {mon.stats.map((value, i) => (
            <span key={STAT_NAMES[i]}>
              {STAT_NAMES[i]} <span className="font-medium text-ink">{value}</span>
            </span>
          ))}
        </div>

        {mon.boosts.some((b) => b !== 0) && (
          <div className="flex flex-wrap gap-1">
            {mon.boosts.map((stage, i) =>
              stage !== 0 ? (
                <span
                  key={BOOST_NAMES[i]}
                  className={`rounded px-1 text-[9px] font-semibold text-white ${
                    stage > 0 ? 'bg-success' : 'bg-danger'
                  }`}
                >
                  {stage > 0 ? '+' : ''}
                  {stage} {BOOST_NAMES[i]}
                </span>
              ) : null,
            )}
          </div>
        )}

        {mon.volatiles.length > 0 && (
          <div className="flex flex-wrap gap-1">
            {mon.volatiles.map((v) => (
              <span key={v.name} className="rounded bg-primary-soft px-1 text-[9px] font-medium text-primary">
                {v.name}
                {v.turns !== undefined ? ` (${v.turns})` : ''}
              </span>
            ))}
          </div>
        )}
      </div>
    </details>
  )
}

export default function TeamInfoSidebar() {
  const view = useBattle((s) => s.view)
  const [tab, setTab] = useState<PlayerId>('p1')

  const side = tab === 'p1' ? view?.p1 : view?.p2
  const active = side?.active ?? []
  const back = side?.back ?? []

  return (
    <aside className="glass flex w-80 shrink-0 flex-col rounded-card">
      <div className="flex border-b border-subtle">
        {(['p1', 'p2'] as PlayerId[]).map((p) => (
          <button
            key={p}
            onClick={() => setTab(p)}
            className={`flex-1 px-3 py-2 text-sm font-semibold ${
              tab === p ? 'border-b-2 border-primary text-primary' : 'text-ink-muted hover:text-ink'
            }`}
          >
            {p === 'p1' ? 'Your Team' : 'Opponent'}
          </button>
        ))}
      </div>

      <div className="flex-1 space-y-1 overflow-y-auto p-2">
        {!side && <p className="p-2 text-xs text-ink-muted">No team data yet.</p>}
        {active.map((mon) => (
          <MonRow key={mon.monId} mon={mon} active />
        ))}
        {back.map((mon) => (
          <MonRow key={mon.monId} mon={mon} active={false} />
        ))}
      </div>
    </aside>
  )
}
