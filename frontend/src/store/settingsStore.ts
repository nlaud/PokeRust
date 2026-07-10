import { create } from 'zustand'
import { computeReadableTextColor } from '../lib/color'

export type Theme = 'light' | 'dark' | 'custom'

const SETTINGS_KEY = 'pokerust.settings.v1'

// Defaults match the previous hardcoded `.custom` CSS class (index.css) so existing
// users see no visual change until they actually pick new colors.
const DEFAULT_CUSTOM_BACKGROUND = '#f5f3ff'
const DEFAULT_CUSTOM_ACCENT = '#7c3aed'

interface Settings {
  theme: Theme
  customBackground: string
  customAccent: string
}

function loadSettings(): Settings {
  try {
    const raw = localStorage.getItem(SETTINGS_KEY)
    if (raw) {
      const parsed = JSON.parse(raw) as Partial<Settings>
      // Backfill fields from before customBackground/customAccent existed.
      return {
        theme: parsed.theme ?? 'light',
        customBackground: parsed.customBackground ?? DEFAULT_CUSTOM_BACKGROUND,
        customAccent: parsed.customAccent ?? DEFAULT_CUSTOM_ACCENT,
      }
    }
  } catch {
    // fall through to defaults
  }
  return { theme: 'light', customBackground: DEFAULT_CUSTOM_BACKGROUND, customAccent: DEFAULT_CUSTOM_ACCENT }
}

// The custom properties `applyCustomColors` overrides — kept as one list so setting
// and clearing them can never drift out of sync with each other.
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

/** Sets the `.custom` theme's CSS custom properties as inline overrides on
 * `<html>` — inline styles win over the `.custom` class rule's static fallback
 * values (index.css), so this cleanly drives the palette from the two user-picked
 * colors without deleting that fallback. Text color is never picked directly — it's
 * computed for contrast against the background (see `computeReadableTextColor`).
 * `--surface-glass`/`--text-secondary`/`--border-subtle`/`--primary-soft` are
 * expressed as `color-mix()` strings (already used elsewhere in index.css for the
 * built-in themes' glass effects) so the mixing math happens in CSS, not JS. */
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
  // Inline custom-color overrides win over every class rule (including .dark), so
  // they must be cleared whenever the active theme isn't 'custom' — otherwise a
  // previously-picked palette bleeds through into light/dark mode.
  if (theme !== 'custom') clearCustomColors()
}

interface SettingsStore extends Settings {
  sidebarOpen: boolean
  setTheme: (theme: Theme) => void
  setCustomColors: (background: string, accent: string) => void
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
    persist({ theme, customBackground: get().customBackground, customAccent: get().customAccent })
    set({ theme })
  },
  setCustomColors: (customBackground, customAccent) => {
    if (get().theme === 'custom') applyCustomColors(customBackground, customAccent)
    persist({ theme: get().theme, customBackground, customAccent })
    set({ customBackground, customAccent })
  },
  setSidebarOpen: (sidebarOpen) => set({ sidebarOpen }),
}))
