import { useEffect, useState } from 'react'
import type { PokemonView } from '../../api/types'
import { useBattle } from '../../store/battleStore'
import MonRow from './MonRow'

type Tab = 'p1' | 'p2' | 'predicates'

export default function TeamInfoSidebar() {
  const view = useBattle((s) => s.view)
  const currentPlayer = useBattle((s) => s.currentPlayer)
  // Show the opponent tab for the current player.
  // Reset this tab after each hotseat handoff.
  const opponentTab: Tab = currentPlayer === 'p1' ? 'p2' : 'p1'
  const [tab, setTab] = useState<Tab>(opponentTab)
  // Do not reset a manual tab choice until the player changes.
  useEffect(() => {
    setTab(opponentTab)
  }, [opponentTab])
  // Use the player and Pokémon ID as the expansion key.
  // This keeps expansion state after a tab change.
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
  // Hide the Predicates tab during perfect information.
  // Select Your Team when a previous battle left this tab active.
  const activeTab: Tab = tab === 'predicates' && !hasBelief ? 'p1' : tab

  const side = activeTab === 'p1' ? view?.p1 : activeTab === 'p2' ? view?.p2 : undefined
  // Team preview supplies masked Pokémon views for both teams.
  // Show these views before the server creates battle sides.
  const previewMons =
    !side && view?.preview && activeTab !== 'predicates'
      ? (activeTab === 'p1' ? view.preview.p1Mons : view.preview.p2Mons)
      : []
  const active = side?.active ?? []
  const back = side?.back ?? previewMons
  const possibleBack = side?.possibleBack ?? []
  // Show replaced fainted opponents from the belief's fainted list.
  // This list keeps their revealed data.
  const faintedBack = side?.fainted ?? []
  // Count brought Pokémon that the player has not seen.
  // Possible species do not change this count.
  // Active, bench, and fainted entries count as seen.
  const hiddenBack = Math.max(
    0,
    (view?.broughtPerSide ?? 0) - (active.length + back.length + faintedBack.length),
  )
  // Put all fainted Pokémon in one section.
  // The source lists do not overlap, so this does not duplicate entries.
  const fainted = [...active, ...back, ...faintedBack].filter((mon) => mon.fainted)
  const liveActive = active.filter((mon) => !mon.fainted)
  const liveBack = back.filter((mon) => !mon.fainted)

  // Use the section and list index as each row key.
  // Unknown bench Pokémon can share a fallback ID.
  // A unique row key prevents shared expansion state.
  const row = (mon: PokemonView, isActive: boolean, section: string, idx: number) => {
    const key = `${activeTab}-${section}-${idx}-${mon.monId}`
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
                {possibleBack
                  .filter((mon) => !mon.fainted)
                  .map((mon, i) => row(mon, false, 'possible', i))}
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
