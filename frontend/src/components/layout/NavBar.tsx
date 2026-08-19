import { NavLink, useLocation } from 'react-router-dom'
import { useSettings } from '../../store/settingsStore'
import { useBattle } from '../../store/battleStore'
import { useSolve } from '../../store/solveStore'
import type { StrategyRow } from '../../api/types'

const tabs = [
  { to: '/', label: 'Teams' },
  { to: '/formats', label: 'Formats' },
  { to: '/simulate', label: 'Simulate' },
  { to: '/tracker', label: 'Tracker' },
]

export default function NavBar() {
  const setSidebarOpen = useSettings((s) => s.setSidebarOpen)
  const location = useLocation()
  const botAnalysis = useBattle((state) => state.botAnalysis)
  const solveLive = useSolve((state) => state.live ?? state.complete)

  const percent = (value: number) => `${(value * 100).toFixed(1)}%`
  const label = (row: StrategyRow | undefined) => {
    if (!row) return 'No move yet'
    if (row.preview) return `Lead ${row.preview.leads.join(' + ')}`
    return row.commands.map((command) => command.description).join(' · ') || 'No action'
  }
  const strategyLabel = (row: StrategyRow | undefined) =>
    row ? `${label(row)} (${percent(row.probability)})` : 'No move yet'
  const summary = location.pathname.startsWith('/simulate') && botAnalysis
    ? `P2 win ${percent(botAnalysis.p2WinOdds)} · ${strategyLabel(botAnalysis.p2Strategy?.rows[0])}${botAnalysis.complete ? '' : ' · partial'}`
    : location.pathname.startsWith('/tracker') && solveLive
      ? `You ${percent(solveLive.p1WinOdds)} · Opp ${percent(solveLive.p2WinOdds)} · ${strategyLabel(solveLive.p1Strategy[0])}${solveLive.complete ? '' : ' · partial'}`
      : null

  return (
    <header className="sticky top-0 z-40 glass">
      <div className="flex h-14 items-center gap-6 px-6">
        <span className="text-lg font-bold tracking-tight text-primary">PokeRust</span>
        <nav className="flex items-center gap-1">
          {tabs.map((tab) => (
            <NavLink
              key={tab.to}
              to={tab.to}
              className={({ isActive }) =>
                `lift rounded-card px-3 py-1.5 text-sm font-medium ${
                  isActive
                    ? 'bg-primary-soft text-primary'
                    : 'text-ink-muted hover:text-ink'
                }`
              }
            >
              {tab.label}
            </NavLink>
          ))}
        </nav>
        {summary && (
          <div
            className="hidden min-w-0 max-w-md flex-1 truncate rounded-card bg-primary-soft px-3 py-1.5 text-xs font-medium text-primary lg:block"
            data-testid="solver-nav-summary"
            title={summary}
          >
            {summary}
          </div>
        )}
        <NavLink
          to="/benchmark"
          className={({ isActive }) =>
            `lift ml-auto rounded-card px-3 py-1.5 text-sm font-medium ${
              isActive ? 'bg-primary-soft text-primary' : 'text-ink-muted hover:text-ink'
            }`
          }
        >
          Benchmark
        </NavLink>
        <button
          onClick={() => setSidebarOpen(true)}
          className="lift rounded-card p-2 text-ink-muted hover:text-ink"
          aria-label="Open settings"
          title="Settings"
        >
          <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
            <circle cx="12" cy="12" r="3" />
            <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 1 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 1 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 1 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 1 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z" />
          </svg>
        </button>
      </div>
      {summary && (
        <div
          className="truncate border-t border-subtle bg-primary-soft px-4 py-1 text-xs font-medium text-primary lg:hidden"
          data-testid="solver-nav-summary-mobile"
          title={summary}
        >
          {summary}
        </div>
      )}
    </header>
  )
}
