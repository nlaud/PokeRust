import { useState, type ReactNode } from 'react'
import MusicPlayer from './MusicPlayer'
import { useSettings, type Theme } from '../../store/settingsStore'
import Select from '../common/Select'
import SolverControls, { DamageRollField } from '../solver/SolverControls'
import {
  IMPERFECT_SOLVERS,
  PERFECT_SOLVERS,
  solverHint,
  supportsRefinement,
  type SolverOption,
  type SolverPreset,
} from '../solver/solverSettings'
import type { BotAlgorithm } from '../../api/types'

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

/** One search dropdown, with what it reads and what the choice does.
 *
 * Two of these cover every search. A session never picks the list, because its
 * information mode already says which information the search may read. */
function SolverPicker({
  label,
  note,
  value,
  options,
  onChange,
  testId,
}: {
  label: string
  note: string
  value: BotAlgorithm
  options: SolverOption[]
  onChange: (algorithm: BotAlgorithm) => void
  testId: string
}) {
  // The hint and the note sit beside the label, never inside it. A label holds
  // the name of its control, and prose inside one both bloats the accessible
  // name and makes a `label:has-text(...)` lookup match the wrong field.
  return (
    <div className="mb-3 text-sm" data-testid={testId}>
      <label className="block">
        <span className="mb-1 block text-ink-muted">{label}</span>
        <Select
          value={value}
          options={options}
          onChange={(next) => onChange(next as BotAlgorithm)}
        />
      </label>
      <p className="mt-1 text-xs text-ink-muted">{solverHint(value)}</p>
      <p className="mt-1 text-[11px] text-ink-muted">{note}</p>
    </div>
  )
}

export default function SettingsSidebar() {
  const {
    sidebarOpen,
    setSidebarOpen,
    theme,
    setTheme,
    customBackground,
    customAccent,
    setCustomColors,
    solverPreset,
    solverSettings,
    imperfectSolver,
    perfectSolver,
    setSolverPreset,
    setSolverSettings,
    setImperfectSolver,
    setPerfectSolver,
  } = useSettings()
  const [advanced, setAdvanced] = useState(false)
  // Every preset runs one turn of lookahead, so the detail names the width that
  // separates them rather than a depth. `SOLVER_PRESETS` holds the values.
  const presets: { value: SolverPreset; label: string; detail: string }[] = [
    { value: 'fast', label: 'Fast', detail: '3 rolls · 12 worlds' },
    { value: 'balanced', label: 'Balanced', detail: '8 rolls · 24 worlds' },
    { value: 'competitive', label: 'High', detail: '16 rolls · 48 worlds' },
  ]

  // Keep the sidebar and MusicPlayer mounted.
  // CSS hides the sidebar without restarting audio.
  return (
    <div className={`fixed inset-0 z-50 ${sidebarOpen ? '' : 'pointer-events-none'}`}>
      <div
        className={`absolute inset-0 bg-black/40 transition-opacity duration-200 ${
          sidebarOpen ? 'opacity-100' : 'opacity-0'
        }`}
        onClick={() => setSidebarOpen(false)}
        aria-hidden
      />
      <aside
        className={`absolute right-0 top-0 flex h-full w-96 max-w-full flex-col overflow-y-auto bg-card p-6 shadow-xl transition-transform duration-200 ${
          sidebarOpen ? 'translate-x-0' : 'translate-x-full'
        }`}
      >
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

          {theme === 'custom' && (
            <div className="mt-3 flex gap-4">
              <label className="flex flex-1 items-center justify-between gap-2 text-sm">
                <span className="text-ink-muted">Background</span>
                <input
                  type="color"
                  value={customBackground}
                  onChange={(e) => setCustomColors(e.target.value, customAccent)}
                  className="h-8 w-12 cursor-pointer rounded-card border border-subtle bg-transparent p-0.5"
                  aria-label="Background color"
                />
              </label>
              <label className="flex flex-1 items-center justify-between gap-2 text-sm">
                <span className="text-ink-muted">Accent</span>
                <input
                  type="color"
                  value={customAccent}
                  onChange={(e) => setCustomColors(customBackground, e.target.value)}
                  className="h-8 w-12 cursor-pointer rounded-card border border-subtle bg-transparent p-0.5"
                  aria-label="Accent color"
                />
              </label>
            </div>
          )}
        </section>

        <section className="mt-6 border-t border-subtle pt-5">
          <h3 className="mb-2 text-sm font-medium text-ink-muted">Solver</h3>
          <SolverPicker
            label="Imperfect-information search"
            note="The tracker always uses it. A simulate battle uses it under every information mode except Perfect Information."
            value={imperfectSolver}
            options={IMPERFECT_SOLVERS}
            onChange={setImperfectSolver}
            testId="imperfect-solver"
          />
          <SolverPicker
            label="Perfect-information search"
            note="A simulate battle uses it under Perfect Information, where neither side hides data."
            value={perfectSolver}
            options={PERFECT_SOLVERS}
            onChange={setPerfectSolver}
            testId="perfect-solver"
          />
          <div className="grid grid-cols-3 gap-2">
            {presets.map((preset) => (
              <button
                key={preset.value}
                type="button"
                onClick={() => setSolverPreset(preset.value)}
                aria-pressed={solverPreset === preset.value}
                // The detail line names the limits, and those values change
                // whenever the preset table changes. Name the button by its
                // preset alone, so a caller never selects it by a limit.
                aria-label={`${preset.label} solver preset`}
                className={`rounded-card border px-2 py-2 text-sm font-medium ${
                  solverPreset === preset.value
                    ? 'border-primary bg-primary-soft text-primary'
                    : 'border-subtle text-ink-muted hover:text-ink'
                }`}
              >
                <span className="block">{preset.label}</span>
                <span className="block text-[10px] font-normal">{preset.detail}</span>
              </button>
            ))}
          </div>
          {solverPreset === 'custom' && (
            <p className="mt-2 text-xs font-medium text-primary">Custom solver limits are active.</p>
          )}
          <p className="mt-3 text-xs text-ink-muted">
            Every preset searches one turn ahead and scores each outcome with the leaf evaluator.
            The damage rolls below decide how much of an attack the search reads.
          </p>
          <div className="mt-2 text-xs">
            <DamageRollField settings={solverSettings} onChange={setSolverSettings} />
          </div>
          <button
            type="button"
            className="mt-3 text-sm font-medium text-ink-muted hover:text-ink"
            onClick={() => setAdvanced(!advanced)}
            aria-expanded={advanced}
          >
            {advanced ? 'Hide advanced limits' : 'Advanced limits'}
          </button>
          {advanced && (
            <div className="text-xs">
              <SolverControls
                algorithm={imperfectSolver}
                // The session picks between the two searches above, so offer
                // refinement when either one can use it.
                showRefine={
                  supportsRefinement(imperfectSolver) || supportsRefinement(perfectSolver)
                }
                settings={solverSettings}
                onChange={setSolverSettings}
              />
            </div>
          )}
        </section>

        <MusicPlayer />
      </aside>
    </div>
  )
}
