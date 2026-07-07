import { NavLink } from 'react-router-dom'
import { useSettings } from '../../store/settingsStore'

const tabs = [
  { to: '/', label: 'Teams' },
  { to: '/formats', label: 'Formats' },
  { to: '/simulate', label: 'Simulate' },
]

export default function NavBar() {
  const setSidebarOpen = useSettings((s) => s.setSidebarOpen)

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
        <button
          onClick={() => setSidebarOpen(true)}
          className="lift ml-auto rounded-card p-2 text-ink-muted hover:text-ink"
          aria-label="Open settings"
          title="Settings"
        >
          <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
            <circle cx="12" cy="12" r="3" />
            <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 1 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 1 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 1 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 1 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z" />
          </svg>
        </button>
      </div>
    </header>
  )
}
