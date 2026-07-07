import { useSettings, type Theme } from '../../store/settingsStore'

const themes: { value: Theme; label: string; hint: string }[] = [
  { value: 'light', label: 'Light', hint: 'Default clean look' },
  { value: 'dark', label: 'Dark', hint: 'Low-light friendly' },
  { value: 'custom', label: 'Custom', hint: 'Violet accent theme' },
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
          <div className="space-y-2">
            {themes.map((option) => (
              <label
                key={option.value}
                className={`lift flex cursor-pointer items-center gap-3 rounded-card border p-3 ${
                  theme === option.value
                    ? 'border-primary bg-primary-soft'
                    : 'border-subtle'
                }`}
              >
                <input
                  type="radio"
                  name="theme"
                  checked={theme === option.value}
                  onChange={() => setTheme(option.value)}
                  className="accent-(--primary)"
                />
                <span>
                  <span className="block text-sm font-medium">{option.label}</span>
                  <span className="block text-xs text-ink-muted">{option.hint}</span>
                </span>
              </label>
            ))}
          </div>
        </section>
      </aside>
    </div>
  )
}
