import { useState } from 'react'
import type { PlayerId, PokemonView } from '../../api/types'
import Sprite from '../../components/common/Sprite'
import { typeStyle } from '../../lib/typeColors'
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

function MonRow({
  mon,
  active,
  expanded,
  onToggle,
}: {
  mon: PokemonView
  active: boolean
  expanded: boolean
  onToggle: () => void
}) {
  const fraction = mon.hp.max > 0 ? mon.hp.current / mon.hp.max : 0

  return (
    <div
      className={`rounded-card transition-colors duration-150 ${active ? 'bg-primary-soft/40' : ''} ${
        mon.fainted ? 'opacity-50' : ''
      } ${expanded ? 'bg-subtle/60' : ''}`}
    >
      <button
        onClick={onToggle}
        aria-expanded={expanded}
        className="flex w-full cursor-pointer items-center gap-2 rounded-card p-2 text-left transition-all duration-150 hover:-translate-y-px hover:bg-primary-soft/60 hover:shadow-sm"
      >
        <Sprite species={mon.species} size={44} />
        <div className="min-w-0 flex-1">
          <div className="flex items-center justify-between">
            <span className="flex min-w-0 items-center gap-1 truncate text-xs font-semibold">
              <span className="truncate">{mon.species}</span>
              {mon.isTera && <span className="text-primary">✦{mon.teraType}</span>}
              {mon.types.map((t) => (
                <span
                  key={t}
                  style={typeStyle(t)}
                  className="shrink-0 rounded px-1 text-[9px] font-medium uppercase"
                >
                  {t}
                </span>
              ))}
            </span>
            <span className="flex items-center gap-1 text-[10px] text-ink-muted">
              Lv{mon.level}
              <svg
                width="12"
                height="12"
                viewBox="0 0 24 24"
                fill="none"
                stroke="currentColor"
                strokeWidth="2"
                strokeLinecap="round"
                strokeLinejoin="round"
                className={`transition-transform duration-200 ${expanded ? 'rotate-180' : ''}`}
              >
                <path d="m6 9 6 6 6-6" />
              </svg>
            </span>
          </div>
          <div className="mt-1 h-2 overflow-hidden rounded-full bg-subtle">
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
      </button>

      {/* Animated expansion: the 0fr → 1fr grid-rows transition animates to the
          content's natural height without measuring it. */}
      <div
        className={`grid transition-[grid-template-rows] duration-200 ease-out ${
          expanded ? 'grid-rows-[1fr]' : 'grid-rows-[0fr]'
        }`}
      >
        <div className="overflow-hidden">
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
              <span className="italic">{mon.nature}</span>
            </div>

            <div className="flex flex-wrap gap-x-2 text-ink-muted">
              {mon.evs.map((value, i) =>
                value !== 0 ? (
                  <span key={STAT_NAMES[i]}>
                    {STAT_NAMES[i]} <span className="font-medium text-ink">{value}</span> EV
                  </span>
                ) : null,
              )}
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
        </div>
      </div>
    </div>
  )
}

export default function TeamInfoSidebar() {
  const view = useBattle((s) => s.view)
  const [tab, setTab] = useState<PlayerId>('p1')
  // Expansion is keyed by player + monId at the sidebar level, so a mon stays
  // expanded when flipping between "Your Team" and "Opponent" and back.
  const [expanded, setExpanded] = useState<Set<string>>(new Set())

  const toggle = (key: string) => {
    setExpanded((prev) => {
      const next = new Set(prev)
      if (next.has(key)) next.delete(key)
      else next.add(key)
      return next
    })
  }

  const side = tab === 'p1' ? view?.p1 : view?.p2
  // During team preview there are no sides yet, but the preview payload
  // already carries full PokemonViews for both teams — show those.
  const previewMons =
    !side && view?.preview
      ? (tab === 'p1' ? view.preview.p1Mons : view.preview.p2Mons)
      : []
  const active = side?.active ?? []
  const back = side?.back ?? previewMons

  const row = (mon: PokemonView, isActive: boolean) => {
    const key = `${tab}-${mon.monId}`
    return (
      <MonRow
        key={key}
        mon={mon}
        active={isActive}
        expanded={expanded.has(key)}
        onToggle={() => toggle(key)}
      />
    )
  }

  return (
    <aside className="glass flex max-h-80 w-full shrink-0 flex-col rounded-card lg:max-h-none lg:w-80">
      <div className="flex border-b border-subtle">
        {(['p1', 'p2'] as PlayerId[]).map((p) => (
          <button
            key={p}
            onClick={() => setTab(p)}
            className={`flex-1 px-3 py-2 text-sm font-semibold transition-colors ${
              tab === p ? 'border-b-2 border-primary text-primary' : 'text-ink-muted hover:text-ink'
            }`}
          >
            {p === 'p1' ? 'Your Team' : 'Opponent'}
          </button>
        ))}
      </div>

      <div className="flex-1 space-y-1 overflow-y-auto p-2">
        {!side && previewMons.length === 0 && (
          <p className="p-2 text-xs text-ink-muted">No team data yet.</p>
        )}
        {active.map((mon) => row(mon, true))}
        {back.map((mon) => row(mon, false))}
      </div>
    </aside>
  )
}
