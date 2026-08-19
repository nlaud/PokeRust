import { create } from 'zustand'
import { computeReadableTextColor } from '../lib/color'
import {
  DEFAULT_SOLVER_SETTINGS,
  SOLVER_PRESETS,
  type SolverPreset,
  type SolverSettings,
} from '../components/solver/solverSettings'

export type Theme = 'light' | 'dark' | 'custom'

const SETTINGS_KEY = 'pokerust.settings.v2'
const OLD_SETTINGS_KEY = 'pokerust.settings.v1'

// Defaults match the old `.custom` colors in `index.css`.
// Existing users see no change before they select new colors.
const DEFAULT_CUSTOM_BACKGROUND = '#f5f3ff'
const DEFAULT_CUSTOM_ACCENT = '#7c3aed'

interface Settings {
  theme: Theme
  customBackground: string
  customAccent: string
  solverPreset: SolverPreset
  solverSettings: SolverSettings
}

function loadSettings(): Settings {
  try {
    const raw = localStorage.getItem(SETTINGS_KEY) ?? localStorage.getItem(OLD_SETTINGS_KEY)
    if (raw) {
      const parsed = JSON.parse(raw) as Partial<Settings>
      // Add custom color fields to old stored settings.
      return {
        theme: parsed.theme ?? 'light',
        customBackground: parsed.customBackground ?? DEFAULT_CUSTOM_BACKGROUND,
        customAccent: parsed.customAccent ?? DEFAULT_CUSTOM_ACCENT,
        solverPreset: parsed.solverPreset ?? 'balanced',
        solverSettings: {
          ...DEFAULT_SOLVER_SETTINGS,
          ...parsed.solverSettings,
          particles: Math.min(32, Math.max(1, parsed.solverSettings?.particles ?? 16)),
        },
      }
    }
  } catch {
    // Use the defaults after an invalid stored value.
  }
  return {
    theme: 'light',
    customBackground: DEFAULT_CUSTOM_BACKGROUND,
    customAccent: DEFAULT_CUSTOM_ACCENT,
    solverPreset: 'balanced',
    solverSettings: DEFAULT_SOLVER_SETTINGS,
  }
}

// Lists each property that `applyCustomColors` changes.
// One list keeps the set and clear operations synchronized.
const CUSTOM_PROPERTIES = [
  '--surface',
  '--surface-card',
  '--surface-glass',
  '--border-subtle',
  '--text-primary',
  '--text-secondary',
  '--primary',
  '--primary-soft',
] as const

function clearCustomColors() {
  const root = document.documentElement.style
  for (const prop of CUSTOM_PROPERTIES) root.removeProperty(prop)
}

/** Applies the custom theme as inline properties on the document element.
 * Inline values override the static `.custom` fallbacks.
 * The code calculates text color from the selected background.
 * CSS `color-mix` expressions calculate the derived colors. */
function applyCustomColors(background: string, accent: string) {
  const text = computeReadableTextColor(background)
  const root = document.documentElement.style
  root.setProperty('--surface', background)
  root.setProperty('--surface-card', background)
  root.setProperty('--surface-glass', `color-mix(in srgb, ${background} 60%, transparent)`)
  root.setProperty('--border-subtle', `color-mix(in srgb, ${text} 10%, transparent)`)
  root.setProperty('--text-primary', text)
  root.setProperty('--text-secondary', `color-mix(in srgb, ${text} 60%, ${background})`)
  root.setProperty('--primary', accent)
  root.setProperty('--primary-soft', `color-mix(in srgb, ${accent} 20%, white)`)
}

function applyTheme(theme: Theme) {
  document.documentElement.classList.remove('dark', 'custom')
  if (theme !== 'light') document.documentElement.classList.add(theme)
  // Inline colors override all theme classes.
  // Clear them when the active theme is not `custom`.
  if (theme !== 'custom') clearCustomColors()
}

interface SettingsStore extends Settings {
  sidebarOpen: boolean
  setTheme: (theme: Theme) => void
  setCustomColors: (background: string, accent: string) => void
  setSolverPreset: (preset: SolverPreset) => void
  setSolverSettings: (settings: SolverSettings) => void
  setSidebarOpen: (open: boolean) => void
}

const initial = loadSettings()
applyTheme(initial.theme)
if (initial.theme === 'custom') applyCustomColors(initial.customBackground, initial.customAccent)

function persist(settings: Settings) {
  localStorage.setItem(SETTINGS_KEY, JSON.stringify(settings))
}

export const useSettings = create<SettingsStore>((set, get) => ({
  ...initial,
  sidebarOpen: false,
  setTheme: (theme) => {
    applyTheme(theme)
    if (theme === 'custom') applyCustomColors(get().customBackground, get().customAccent)
    persist({ ...get(), theme })
    set({ theme })
  },
  setCustomColors: (customBackground, customAccent) => {
    if (get().theme === 'custom') applyCustomColors(customBackground, customAccent)
    persist({ ...get(), customBackground, customAccent })
    set({ customBackground, customAccent })
  },
  setSolverPreset: (solverPreset) => {
    const solverSettings =
      solverPreset === 'custom' ? get().solverSettings : { ...SOLVER_PRESETS[solverPreset] }
    persist({ ...get(), solverPreset, solverSettings })
    set({ solverPreset, solverSettings })
  },
  setSolverSettings: (solverSettings) => {
    const safe = { ...solverSettings, particles: Math.min(32, Math.max(1, solverSettings.particles)) }
    persist({ ...get(), solverPreset: 'custom', solverSettings: safe })
    set({ solverPreset: 'custom', solverSettings: safe })
  },
  setSidebarOpen: (sidebarOpen) => set({ sidebarOpen }),
}))
