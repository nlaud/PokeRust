import type { ReactNode } from 'react'
import { useSettings, type Theme } from '../../store/settingsStore'

const sunIcon = (
  <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
    <circle cx="12" cy="12" r="4" />
    <path d="M12 2v2m0 16v2M4.9 4.9l1.4 1.4m11.4 11.4 1.4 1.4M2 12h2m16 0h2M4.9 19.1l1.4-1.4m11.4-11.4 1.4-1.4" />
  </svg>
)
const moonIcon = (
  <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
    <path d="M21 12.8A9 9 0 1 1 11.2 3 7 7 0 0 0 21 12.8z" />
  </svg>
)
const paletteIcon = (
  <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
    <path d="M12 22a10 10 0 1 1 10-10c0 2.2-1.8 3-3 3h-2.5a2.5 2.5 0 0 0-1.8 4.3c.4.4.6.9.6 1.4 0 .7-.6 1.3-1.3 1.3z" />
    <circle cx="7.5" cy="11.5" r="1" />
    <circle cx="12" cy="7.5" r="1" />
    <circle cx="16.5" cy="11.5" r="1" />
  </svg>
)

const themes: { value: Theme; label: string; icon: ReactNode }[] = [
  { value: 'light', label: 'Light', icon: sunIcon },
  { value: 'dark', label: 'Dark', icon: moonIcon },
  { value: 'custom', label: 'Custom', icon: paletteIcon },
]

export default function SettingsSidebar() {
  const { sidebarOpen, setSidebarOpen, theme, setTheme } = useSettings()

  if (!sidebarOpen) return null

  return (
    <div className="fixed inset-0 z-50">
      <div
        className="absolute inset-0 bg-black/40"
        onClick={() => setSidebarOpen(false)}
        aria-hidden
      />
      <aside className="absolute right-0 top-0 h-full w-80 bg-card p-6 shadow-xl">
        <div className="mb-6 flex items-center justify-between">
          <h2 className="text-lg font-semibold">Settings</h2>
          <button
            onClick={() => setSidebarOpen(false)}
            className="lift rounded-card p-1.5 text-ink-muted hover:text-ink"
            aria-label="Close settings"
          >
            <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
              <path d="M18 6 6 18M6 6l12 12" />
            </svg>
          </button>
        </div>

        <section>
          <h3 className="mb-2 text-sm font-medium text-ink-muted">Display</h3>
          <div className="flex overflow-hidden rounded-card border border-subtle">
            {themes.map((option) => (
              <button
                key={option.value}
                onClick={() => setTheme(option.value)}
                aria-pressed={theme === option.value}
                className={`flex flex-1 items-center justify-center gap-1.5 px-2 py-2 text-sm font-medium transition-colors ${
                  theme === option.value
                    ? 'bg-primary text-white'
                    : 'text-ink-muted hover:bg-primary-soft hover:text-ink'
                }`}
              >
                {option.icon}
                {option.label}
              </button>
            ))}
          </div>
        </section>
      </aside>
    </div>
  )
}
