import type { BotAlgorithm } from '../../api/types'
import { isBeliefSearch, supportsRefinement, type SolverSettings } from './solverSettings'

function NumberField({
  label,
  value,
  min,
  max,
  disabled,
  onChange,
}: {
  label: string
  value: number
  min: number
  max: number
  disabled?: boolean
  onChange: (value: number) => void
}) {
  return (
    <label className="block min-w-[8rem] flex-1">
      <span className="mb-0.5 block text-ink-muted">{label}</span>
      <input
        type="number"
        value={value}
        min={min}
        max={max}
        disabled={disabled}
        onChange={(event) => {
          const next = Number(event.target.value)
          if (Number.isFinite(next)) onChange(Math.max(min, Math.min(max, Math.trunc(next))))
        }}
        className="w-full rounded-card border border-subtle bg-card px-2 py-2 text-ink disabled:opacity-50"
      />
    </label>
  )
}

/** Shows the solver limits that have a clear speed or precision effect. */
export default function SolverControls({
  algorithm,
  settings,
  disabled,
  showRefine,
  onChange,
}: {
  algorithm?: BotAlgorithm
  settings: SolverSettings
  disabled?: boolean
  /** Whether to offer the refinement switch.
   *
   * Refinement is one shared setting, and two pickers feed it: the sidebar
   * holds an imperfect-information search and a perfect-information search, and
   * the session picks between them. `algorithm` names only one of the two, so
   * the caller decides. An absent value falls back to `algorithm`. */
  showRefine?: boolean
  onChange: (settings: SolverSettings) => void
}) {
  const set = <K extends keyof SolverSettings>(key: K, value: SolverSettings[K]) =>
    onChange({ ...settings, [key]: value })

  return (
    <div className="mt-2 grid grid-cols-2 gap-2 sm:grid-cols-3">
      <label className="block min-w-[8rem] flex-1">
        <span className="mb-0.5 block text-ink-muted">Simulation turns</span>
        <input
          type="number"
          value={settings.simulationTurnBudget}
          min={1}
          max={1_000_000_000}
          disabled={disabled}
          onChange={(event) => {
            const next = Number(event.target.value)
            if (Number.isFinite(next))
              set('simulationTurnBudget', Math.max(1, Math.min(1_000_000_000, Math.trunc(next))))
          }}
          className="w-full rounded-card border border-subtle bg-card px-2 py-2 text-ink disabled:opacity-50"
        />
      </label>
      <NumberField
        label="Depth"
        value={settings.depth}
        min={1}
        max={8}
        disabled={disabled}
        onChange={(value) => set('depth', value)}
      />
      <label className="block min-w-[8rem] flex-1">
        <span className="mb-0.5 block text-ink-muted">Replacement depth</span>
        <input
          type="number"
          value={settings.replacementDepth ?? ''}
          min={1}
          max={8}
          disabled={disabled || settings.replacementDepth === null}
          onChange={(event) => {
            const next = Number(event.target.value)
            if (Number.isFinite(next)) set('replacementDepth', Math.max(1, Math.min(8, Math.trunc(next))))
          }}
          className="w-full rounded-card border border-subtle bg-card px-2 py-2 text-ink disabled:opacity-50"
        />
        <span className="mt-1 flex items-center gap-1 text-ink-muted">
          <input
            type="checkbox"
            checked={settings.replacementDepth === null}
            disabled={disabled}
            onChange={(event) => set('replacementDepth', event.target.checked ? null : 2)}
          />
          Use remaining depth
        </span>
      </label>
      <NumberField
        label="Damage rolls"
        value={settings.damageRolls}
        min={1}
        max={16}
        disabled={disabled}
        onChange={(value) => set('damageRolls', value)}
      />
      {(algorithm === undefined || isBeliefSearch(algorithm)) && (
        <NumberField
          label="Particles"
          value={settings.particles}
          min={1}
          max={32}
          disabled={disabled}
          onChange={(value) => set('particles', value)}
        />
      )}
      <label className="flex items-center gap-2 self-center">
        <input
          type="checkbox"
          checked={settings.considerCrit}
          disabled={disabled}
          onChange={(event) => set('considerCrit', event.target.checked)}
        />
        Critical-hit branches
      </label>
      {(showRefine ?? (algorithm === undefined || supportsRefinement(algorithm))) && (
        <label
          className="flex items-center gap-2 self-center"
          title="Solves depth 1 over every action, then raises only the cells that decide the answer to the chosen depth. A doubles position cannot finish a complete depth-2 search, and this does reach depth 2 on the cells that matter. The answer reports how many actions it verified."
        >
          <input
            type="checkbox"
            checked={settings.refine}
            disabled={disabled}
            onChange={(event) => set('refine', event.target.checked)}
          />
          Refine to depth
        </label>
      )}
    </div>
  )
}
