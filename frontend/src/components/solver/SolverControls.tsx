import type { BotAlgorithm } from '../../api/types'
import {
  autoSimulationTurnBudget,
  isBeliefSearch,
  type SolverSettings,
} from './solverSettings'

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
  onChange,
}: {
  algorithm: BotAlgorithm
  settings: SolverSettings
  disabled?: boolean
  onChange: (settings: SolverSettings) => void
}) {
  const set = <K extends keyof SolverSettings>(key: K, value: SolverSettings[K]) =>
    onChange({ ...settings, [key]: value })

  // The budget that the server derives while the field stays on "Scale
  // automatically". Showing it keeps the number in front of the reader, and it
  // seeds the field when the reader takes the budget over.
  const autoBudget = autoSimulationTurnBudget(algorithm, settings)

  return (
    <div className="mt-2 grid grid-cols-2 gap-2 sm:grid-cols-3">
      <label className="block min-w-[8rem] flex-1">
        <span className="mb-0.5 block text-ink-muted">Simulation turns</span>
        <input
          type="number"
          value={settings.simulationTurnBudget ?? autoBudget}
          min={1}
          max={1_000_000_000}
          disabled={disabled || settings.simulationTurnBudget === null}
          onChange={(event) => {
            const next = Number(event.target.value)
            if (Number.isFinite(next))
              set('simulationTurnBudget', Math.max(1, Math.min(1_000_000_000, Math.trunc(next))))
          }}
          className="w-full rounded-card border border-subtle bg-card px-2 py-2 text-ink disabled:opacity-50"
        />
        <span className="mt-1 flex items-center gap-1 text-ink-muted">
          <input
            type="checkbox"
            checked={settings.simulationTurnBudget === null}
            disabled={disabled}
            onChange={(event) =>
              set('simulationTurnBudget', event.target.checked ? null : autoBudget)
            }
          />
          Scale automatically
        </span>
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
      {isBeliefSearch(algorithm) && (
        <NumberField
          label="Particles"
          value={settings.particles}
          min={1}
          max={512}
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
    </div>
  )
}
