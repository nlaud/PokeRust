import { useState } from 'react'
import type { PokemonView } from '../../api/types'
import MonRow from '../simulate/MonRow'
import { useTracker } from '../../store/trackerStore'

// Recreates `pages/simulate/TeamInfoSidebar.tsx`'s tab shell and roster
// derivation for tracker mode, reusing the same `MonRow` (item/ability/
// nature/EVs/stat-ranges/boosts/volatiles — exactly what the fog-of-war
// engine narrows) so a tracked opponent's belief visibly tightens the same
// way an in-progress simulated battle's does. Two differences from battle
// mode's sidebar:
//   - No `currentPlayer`-driven tab auto-switching (tracker has no hotseat
//     handoff between two players) — the default tab is just a constant.
//   - The two roster tabs are labeled "Your Team"/"Opponent", not
//     "Player 1"/"Player 2" — tracker mode is always one fixed perspective.

type Tab = 'p1' | 'p2' | 'predicates'

export default function TrackerTeamSidebar() {
  const view = useTracker((s) => s.view)
  const [tab, setTab] = useState<Tab>('p2')
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

  const hasBelief = view?.belief !== undefined
  // The Predicates tab only exists under a non-Perfect-Information mode; fall
  // back to "Your Team" if it was selected in a previous session that's
  // since ended (tracker mode never runs Perfect Information — see
  // `CreateTrackerRequest.informationMode` — but a stale tab pick from a
  // prior session is still worth guarding the same way battle mode does).
  const activeTab: Tab = tab === 'predicates' && !hasBelief ? 'p1' : tab

  const side = activeTab === 'p1' ? view?.p1 : activeTab === 'p2' ? view?.p2 : undefined
  const active = side?.active ?? []
  const back = side?.back ?? []
  const possibleBack = side?.possibleBack ?? []
  // Opponent mons that fainted and were then replaced: the fog belief tracks
  // these in their own bucket (rather than dropping them) so their revealed
  // info survives the switch that benched them.
  const faintedBack = side?.fainted ?? []
  // Real hidden-slot count (not the possibleBack candidate-species count):
  // how many of this side's brought mons we simply haven't seen yet.
  const hiddenBack = Math.max(
    0,
    (view?.broughtPerSide ?? 0) - (active.length + back.length + faintedBack.length),
  )
  const fainted = [...active, ...back, ...faintedBack].filter((mon) => mon.fainted)
  const liveActive = active.filter((mon) => !mon.fainted)
  const liveBack = back.filter((mon) => !mon.fainted)

  // Keyed by section + list index, not just `mon.monId` — see
  // `TeamInfoSidebar.tsx`'s identical comment for why (bench mons whose
  // identity hasn't narrowed yet can share a fallback id).
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
          Your Team
        </button>
        <button
          onClick={() => setTab('p2')}
          className={`flex-1 px-3 py-2 text-sm font-semibold transition-colors ${
            activeTab === 'p2' ? 'border-b-2 border-primary text-primary' : 'text-ink-muted hover:text-ink'
          }`}
        >
          Opponent
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
            {!side && <p className="p-2 text-xs text-ink-muted">No team data yet.</p>}
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
