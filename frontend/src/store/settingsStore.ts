import { create } from 'zustand'

export type Theme = 'light' | 'dark' | 'custom'

const SETTINGS_KEY = 'pokerust.settings.v1'

interface Settings {
  theme: Theme
}

function loadSettings(): Settings {
  try {
    const raw = localStorage.getItem(SETTINGS_KEY)
    if (raw) return JSON.parse(raw) as Settings
  } catch {
    // fall through to defaults
  }
  return { theme: 'light' }
}

function applyTheme(theme: Theme) {
  document.documentElement.classList.remove('dark', 'custom')
  if (theme !== 'light') document.documentElement.classList.add(theme)
}

interface SettingsStore extends Settings {
  sidebarOpen: boolean
  setTheme: (theme: Theme) => void
  setSidebarOpen: (open: boolean) => void
}

const initial = loadSettings()
applyTheme(initial.theme)

export const useSettings = create<SettingsStore>((set) => ({
  ...initial,
  sidebarOpen: false,
  setTheme: (theme) => {
    applyTheme(theme)
    localStorage.setItem(SETTINGS_KEY, JSON.stringify({ theme }))
    set({ theme })
  },
  setSidebarOpen: (sidebarOpen) => set({ sidebarOpen }),
}))
