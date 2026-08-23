import type { BotAlgorithm } from '../../api/types'
import {
  MAX_PARTICLES,
  isBeliefSearch,
  supportsRefinement,
  type SolverSettings,
} from './solverSettings'

function NumberField({
  label,
  title,
  value,
  min,
  max,
  disabled,
  onChange,
}: {
  label: string
  title?: string
  value: number
  min: number
  max: number
  disabled?: boolean
  onChange: (value: number) => void
}) {
  return (
    <label className="block min-w-0" title={title}>
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

/** The damage rolls that every attack of one search reads.
 *
 * An attack rolls one of sixteen damage values. A search that reads fewer of
 * them scores an attack from a sample of its outcomes, so it can miss the roll
 * that faints a target and the roll that does not.
 *
 * This limit is the main precision control of a depth-one search, so it sits
 * beside the presets rather than inside the advanced drawer. It is also the
 * most expensive one: `benches/RESULTS.md` records that a doubles position
 * needs about 14,000 turn simulations at one roll and about 1,340,000 at
 * sixteen, because each extra roll makes more branches that faint a Pokemon and
 * each faint opens a replacement search. */
export function DamageRollField({
  settings,
  disabled,
  onChange,
}: {
  settings: SolverSettings
  disabled?: boolean
  onChange: (settings: SolverSettings) => void
}) {
  return (
    <NumberField
      label="Damage rolls (1-16)"
      title="An attack rolls one of sixteen damage values. A search that reads fewer of them can miss the roll that faints a target. In doubles this is the most expensive limit: sixteen rolls cost about 93 times one roll."
      value={settings.damageRolls}
      min={1}
      max={16}
      disabled={disabled}
      onChange={(value) => onChange({ ...settings, damageRolls: value })}
    />
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
      <NumberField
        label="Simulation turns, exact"
        title="Double Oracle, Serialized Bounds, Backward Induction, and PIMC stop when the matrix is solved. This budget has to hold that solve. A smaller one returns a matrix of static scores, which names no action."
        value={settings.simulationTurnBudget}
        min={1}
        max={1000000000}
        disabled={disabled}
        onChange={(value) => set('simulationTurnBudget', value)}
      />
      <NumberField
        label="Simulation turns, sampled"
        title="ISMCTS, MCCFR, and MCTS spend every turn they are given, so this sets how many seconds one answer takes rather than whether the answer is complete."
        value={settings.sampledSimulationTurnBudget}
        min={1}
        max={1000000000}
        disabled={disabled}
        onChange={(value) => set('sampledSimulationTurnBudget', value)}
      />
      <NumberField
        label="Depth"
        value={settings.depth}
        min={1}
        max={8}
        disabled={disabled}
        onChange={(value) => set('depth', value)}
      />
      <label className="block min-w-0">
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
      {(algorithm === undefined || isBeliefSearch(algorithm)) && (
        <NumberField
          label="Particles"
          value={settings.particles}
          min={1}
          max={MAX_PARTICLES}
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
