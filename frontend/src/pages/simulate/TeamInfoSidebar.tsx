import { useEffect, useState } from 'react'
import type { PokemonView } from '../../api/types'
import Sprite from '../../components/common/Sprite'
import { hpDisplayText, hpFraction } from '../../lib/hp'
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

/** The server sends the raw scaled 0–252 EV (`scale_evs_for_stat_points` in
 * poke_rust/src/state/pokemon.rs: `ev = max(0, 8*points - 4)`) — teamsheets are
 * authored in 0–32 stat points, so invert that to show the sheet author's own units. */
function evToStatPoints(ev: number): number {
  return ev === 0 ? 0 : Math.round((ev + 4) / 8)
}

/** A range value: shows a single number when the bound has collapsed to a point
 * (ground truth, or a masked value that's since been narrowed to certainty), or
 * "min–max" while genuinely uncertain. */
function RangeValue({ min, max }: { min: number; max: number }) {
  return <>{min === max ? min : `${min}–${max}`}</>
}

/** Splits a masked item string back into its list entries and the separator that
 * joined them, mirroring the two join styles `describe_unknown_item` emits on the
 * server: " or " for a possible-item list, ", " for an exclusion list ("not X, not
 * Y"). A single value (Known, or "Unknown") has no separator and is left whole. */
function splitItemList(text: string): { parts: string[]; sep: string } {
  if (text.includes(' or ')) return { parts: text.split(' or '), sep: ' or ' }
  if (text.includes(', ')) return { parts: text.split(', '), sep: ', ' }
  return { parts: [text], sep: '' }
}

/** Item text with a click-to-expand truncation past 3 entries, per the masked-item
 * display convention: show whichever of possible/impossible is shorter, but never
 * dump a long list inline. */
function ItemText({ item }: { item: string | null }) {
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

function MonRow({
  mon,
  active,
  expanded,
  onToggle,
  grayed = false,
}: {
  mon: PokemonView
  active: boolean
  expanded: boolean
  onToggle: () => void
  /** Possible-back-mons rendering: no expand affordance, "we have no info on them"
   * beyond species — just a grayed-out roster icon. */
  grayed?: boolean
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
            {!grayed && (
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
            )}
          </span>
        </div>
        {!grayed && (
          <>
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
          </>
        )}
      </div>
    </>
  )

  if (grayed) {
    return (
      <div className="flex w-full items-center gap-2 rounded-card p-2 text-left opacity-50">
        {header}
      </div>
    )
  }

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

type Tab = 'p1' | 'p2' | 'predicates'

export default function TeamInfoSidebar() {
  const view = useBattle((s) => s.view)
  const currentPlayer = useBattle((s) => s.currentPlayer)
  // Default (and re-default) to whichever tab is NOT the player we're currently
  // watching from — i.e. the opposing team — so the hotseat handoff always opens
  // on the newly-active player's view of their opponent, not a stale manual pick.
  const opponentTab: Tab = currentPlayer === 'p1' ? 'p2' : 'p1'
  const [tab, setTab] = useState<Tab>(opponentTab)
  // Only re-trigger when the watched perspective actually changes — a manual
  // tab click within the same perspective must not be clobbered.
  useEffect(() => {
    setTab(opponentTab)
  }, [opponentTab])
  // Expansion is keyed by player + monId at the sidebar level, so a mon stays
  // expanded when flipping between "Player 1" and "Player 2" and back.
  const [expanded, setExpanded] = useState<Set<string>>(new Set())

  const toggle = (key: string) => {
    setExpanded((prev) => {
      const next = new Set(prev)
      if (next.has(key)) next.delete(key)
      else next.add(key)
      return next
    })
  }

  const hasBelief = view?.belief !== undefined
  // The Predicates tab only exists under a non-Perfect-Information mode; fall back
  // to "Your Team" if it was selected in a previous battle that's since ended.
  const activeTab: Tab = tab === 'predicates' && !hasBelief ? 'p1' : tab

  const side = activeTab === 'p1' ? view?.p1 : activeTab === 'p2' ? view?.p2 : undefined
  // During team preview there are no sides yet, but the preview payload already
  // carries PokemonViews for both teams — the server masks p2Mons the same way it
  // masks a battle-phase SideView (see `preview_view` in mapping.rs), so this is
  // safe to show as-is even for the opponent tab.
  const previewMons =
    !side && view?.preview && activeTab !== 'predicates'
      ? (activeTab === 'p1' ? view.preview.p1Mons : view.preview.p2Mons)
      : []
  const active = side?.active ?? []
  const back = side?.back ?? previewMons
  const possibleBack = side?.possibleBack ?? []
  // Real hidden-slot count (not the possibleBack candidate-species count): how many
  // of this side's brought mons we simply haven't seen yet. Decreases as mons are
  // revealed and hits 0 once every brought mon has been seen (even if some brought
  // species are still ambiguous within `possibleBack`).
  const hiddenBack = Math.max(0, (view?.broughtPerSide ?? 0) - (active.length + back.length))
  // Fainted mons get pulled out of the active/back lists into their own section
  // below "Possibly in the back" — grouping them together (rather than leaving them
  // dimmed in place) surfaces their revealed info in one predictable spot.
  const fainted = [...active, ...back].filter((mon) => mon.fainted)
  const liveActive = active.filter((mon) => !mon.fainted)
  const liveBack = back.filter((mon) => !mon.fainted)

  // Keyed by section + list index, not just `mon.monId`: bench mons whose identity
  // hasn't narrowed yet can share a fallback id (or, historically, all shared the
  // same placeholder id — see mapping.rs's `bench_pokemon_view_from_belief`), and a
  // duplicate React key merges those rows' expansion state and corrupts
  // reconciliation across tab switches. The section+index pair is always unique
  // within one render regardless of what the belief currently knows about monId.
  const row = (mon: PokemonView, isActive: boolean, section: string, idx: number, grayed = false) => {
    const key = `${activeTab}-${section}-${idx}-${mon.monId}`
    return (
      <MonRow
        key={key}
        mon={mon}
        active={isActive}
        expanded={expanded.has(key)}
        onToggle={() => toggle(key)}
        grayed={grayed}
      />
    )
  }

  return (
    <aside className="glass flex max-h-80 w-full shrink-0 flex-col rounded-card lg:max-h-none lg:w-80">
      <div className="flex border-b border-subtle">
        <button
          onClick={() => setTab('p1')}
          className={`flex-1 px-3 py-2 text-sm font-semibold transition-colors ${
            activeTab === 'p1' ? 'border-b-2 border-primary text-primary' : 'text-ink-muted hover:text-ink'
          }`}
        >
          Player 1
        </button>
        <button
          onClick={() => setTab('p2')}
          className={`flex-1 px-3 py-2 text-sm font-semibold transition-colors ${
            activeTab === 'p2' ? 'border-b-2 border-primary text-primary' : 'text-ink-muted hover:text-ink'
          }`}
        >
          Player 2
        </button>
        {hasBelief && (
          <button
            onClick={() => setTab('predicates')}
            className={`flex-1 px-3 py-2 text-sm font-semibold transition-colors ${
              activeTab === 'predicates'
                ? 'border-b-2 border-primary text-primary'
                : 'text-ink-muted hover:text-ink'
            }`}
          >
            Predicates
          </button>
        )}
      </div>

      <div className="flex-1 space-y-1 overflow-y-auto p-2">
        {activeTab === 'predicates' ? (
          view?.belief && view.belief.clauses.length > 0 ? (
            <ul className="space-y-1.5 p-1 text-xs">
              {view.belief.clauses.map((clause, i) => (
                <li key={i} className="rounded-card bg-subtle/60 px-2 py-1.5 leading-snug">
                  {clause}
                </li>
              ))}
            </ul>
          ) : (
            <p className="p-2 text-xs text-ink-muted">No deductions yet.</p>
          )
        ) : (
          <>
            {!side && previewMons.length === 0 && (
              <p className="p-2 text-xs text-ink-muted">No team data yet.</p>
            )}
            {liveActive.map((mon, i) => row(mon, true, 'active', i))}
            {liveBack.map((mon, i) => row(mon, false, 'back', i))}
            {hiddenBack > 0 && (
              <>
                <p className="px-1 pt-2 text-[10px] font-semibold uppercase tracking-wide text-ink-muted">
                  Possibly in the back ({hiddenBack})
                </p>
                {possibleBack.map((mon, i) => row(mon, false, 'possible', i, true))}
              </>
            )}
            {fainted.length > 0 && (
              <>
                <p className="px-1 pt-2 text-[10px] font-semibold uppercase tracking-wide text-ink-muted">
                  Fainted
                </p>
                {fainted.map((mon, i) => row(mon, false, 'fainted', i))}
              </>
            )}
          </>
        )}
      </div>
    </aside>
  )
}
