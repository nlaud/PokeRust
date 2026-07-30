import { useState } from 'react'
import type { PokemonView } from '../../api/types'
import Sprite from '../../components/common/Sprite'
import { hpDisplayText, hpFraction } from '../../lib/hp'
import { typeStyle } from '../../lib/typeColors'

// Shows the hidden-information fields for one Pokémon.
// Props provide all data, so simulator and tracker sidebars can use this component.

export const STAT_NAMES = ['HP', 'Atk', 'Def', 'SpA', 'SpD', 'Spe']
export const BOOST_NAMES = ['Atk', 'Def', 'SpA', 'SpD', 'Spe', 'Acc', 'Eva']

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

/** Converts a scaled EV to the stat points used in teamsheets. */
export function evToStatPoints(ev: number): number {
  return ev === 0 ? 0 : Math.round((ev + 4) / 8)
}

/** Shows one exact value or a range. */
export function RangeValue({ min, max }: { min: number; max: number }) {
  return <>{min === max ? min : `${min}–${max}`}</>
}

/** Splits a masked item list and keeps its separator. */
export function splitItemList(text: string): { parts: string[]; sep: string } {
  if (text.includes(' or ')) return { parts: text.split(' or '), sep: ' or ' }
  if (text.includes(', ')) return { parts: text.split(', '), sep: ', ' }
  return { parts: [text], sep: '' }
}

/** Truncates item lists after three entries.
 * The user can expand the complete list. */
export function ItemText({ item }: { item: string | null }) {
  const [expanded, setExpanded] = useState(false)
  if (item == null) return <>None</>

  const { parts, sep } = splitItemList(item)
  if (parts.length <= 3) return <>{item}</>

  if (expanded) {
    return (
      <>
        {item}{' '}
        <button
          onClick={(e) => {
            e.stopPropagation()
            setExpanded(false)
          }}
          className="text-primary underline"
        >
          (show less)
        </button>
      </>
    )
  }
  const shown = parts.slice(0, 3).join(sep)
  return (
    <>
      {shown}{' '}
      <button
        onClick={(e) => {
          e.stopPropagation()
          setExpanded(true)
        }}
        className="text-primary underline"
      >
        +{parts.length - 3} more
      </button>
    </>
  )
}

export default function MonRow({
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
  const fraction = hpFraction(mon.hp, mon.statsMax[0])

  const header = (
    <>
      <Sprite species={mon.species} size={44} />
      <div className="min-w-0 flex-1">
        <div className="flex items-center justify-between">
          <span className="flex min-w-0 items-center gap-1 truncate text-xs font-semibold">
            <span className="truncate">{mon.species}</span>
            {mon.isIllusionSuspected && (
              <span title="Could still be an Illusion disguise — not yet ruled out" className="text-warning">
                ⚠
              </span>
            )}
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
          <span className="text-[10px] text-ink-muted">{hpDisplayText(mon.hp, mon.statsMax[0])}</span>
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
    </>
  )

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
        {header}
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
                <span className="font-medium text-ink">Item:</span> <ItemText item={mon.item} />
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
                  {STAT_NAMES[i]}{' '}
                  <span className="font-medium text-ink">
                    <RangeValue min={value} max={mon.statsMax[i]} />
                  </span>
                </span>
              ))}
              <span className="italic">{mon.nature}</span>
            </div>

            <div className="flex flex-wrap gap-x-2 text-ink-muted">
              {mon.evs.map((value, i) =>
                value !== 0 || mon.evsMax[i] !== value ? (
                  <span key={STAT_NAMES[i]}>
                    {STAT_NAMES[i]}{' '}
                    <span className="font-medium text-ink">
                      <RangeValue min={evToStatPoints(value)} max={evToStatPoints(mon.evsMax[i])} />
                    </span>{' '}
                    SP
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
