import type { PokemonView } from '../../api/types'

const STATUS_COLORS: Record<string, string> = {
  BRN: 'bg-orange-500',
  PSN: 'bg-purple-500',
  TOX: 'bg-purple-700',
  PAR: 'bg-yellow-500',
  SLP: 'bg-slate-400',
  FRZ: 'bg-cyan-400',
}

const BOOST_NAMES = ['Atk', 'Def', 'SpA', 'SpD', 'Spe', 'Acc', 'Eva']

function hpBarColor(fraction: number): string {
  if (fraction <= 0.2) return 'bg-danger'
  if (fraction <= 0.5) return 'bg-warning'
  return 'bg-success'
}

export default function PokemonHUD({ mon }: { mon: PokemonView }) {
  const fraction = mon.hp.max > 0 ? mon.hp.current / mon.hp.max : 0

  return (
    <div className={`glass w-full rounded-card p-2.5 shadow-sm ${mon.fainted ? 'opacity-50' : ''}`}>
      <div className="flex items-center justify-between">
        <span className="truncate text-sm font-semibold">
          {mon.species}
          {mon.isTera && <span className="ml-1 text-primary">✦{mon.teraType}</span>}
        </span>
        <span className="text-[11px] text-ink-muted">Lv{mon.level}</span>
      </div>

      <div className="mt-1 h-3 overflow-hidden rounded-full bg-subtle">
        <div
          className={`h-full rounded-full transition-all duration-500 ${hpBarColor(fraction)}`}
          style={{ width: `${Math.max(0, fraction) * 100}%` }}
        />
      </div>
      <div className="mt-0.5 flex items-center justify-between">
        <span className="text-xs text-ink-muted">
          {mon.hp.current}/{mon.hp.max}
        </span>
        {mon.status && (
          <span
            className={`rounded px-1 text-[9px] font-bold text-white ${STATUS_COLORS[mon.status.code] ?? 'bg-slate-500'}`}
          >
            {mon.status.code}
          </span>
        )}
      </div>

      {(mon.boosts.some((b) => b !== 0) || mon.volatiles.length > 0) && (
        <div className="mt-1 flex flex-wrap gap-1">
          {mon.boosts.map((stage, i) =>
            stage !== 0 ? (
              <span
                key={BOOST_NAMES[i]}
                className={`rounded px-1 text-[9px] font-semibold text-white ${stage > 0 ? 'bg-success' : 'bg-danger'}`}
              >
                {stage > 0 ? '+' : ''}
                {stage} {BOOST_NAMES[i]}
              </span>
            ) : null,
          )}
          {mon.volatiles.map((v) => (
            <span key={v.name} className="rounded bg-primary-soft px-1 text-[9px] font-medium text-primary">
              {v.name}
              {v.turns !== undefined ? ` (${v.turns})` : ''}
            </span>
          ))}
        </div>
      )}
    </div>
  )
}
