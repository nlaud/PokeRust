import type { BotAlgorithm } from '../../api/types'
import {
  MAX_PARTICLES,
  PRESET_DEPTH,
  enumeratesTurnBranches,
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
 * What a roll costs depends on the search, so this field edits one of two
 * values and `algorithm` decides which.
 *
 * An exact search, PIMC, and MCTS build every branch of a turn, so each roll
 * multiplies the work. `benches/RESULTS.md` records a doubles position at
 * 14,482 turn simulations for one roll and 1,347,463 for sixteen, because each
 * extra roll makes more branches that faint a Pokemon and each faint opens a
 * replacement search.
 *
 * ISMCTS and MCCFR draw one outcome for each turn, so a roll only widens the
 * set that the draw comes from. Sixteen rolls cost those searches 1.6 times one
 * roll, against 3,400 times for MCTS, so every preset reads all sixteen there. */
export function DamageRollField({
  algorithm,
  settings,
  disabled,
  onChange,
}: {
  algorithm?: BotAlgorithm
  settings: SolverSettings
  disabled?: boolean
  onChange: (settings: SolverSettings) => void
}) {
  const sampled = algorithm !== undefined && !enumeratesTurnBranches(algorithm)
  return (
    <NumberField
      label={sampled ? 'Damage rolls, sampled (1-16)' : 'Damage rolls, exact (1-16)'}
      title={
        sampled
          ? 'ISMCTS and MCCFR draw one outcome for each turn, so a damage roll widens the set that the draw comes from rather than multiplying the work. Sixteen rolls cost these searches 1.6 times one roll, against 3,400 times for MCTS. One roll makes every attack deal its average damage, so the search cannot tell a roll that faints a target from one that does not.'
          : 'An attack rolls one of sixteen damage values. A search that reads fewer of them can miss the roll that faints a target. For a search that enumerates a turn this is the most expensive limit: sixteen rolls cost about 93 times one roll in doubles.'
      }
      value={sampled ? settings.sampledDamageRolls : settings.damageRolls}
      min={1}
      max={16}
      disabled={disabled}
      onChange={(value) =>
        onChange(
          sampled ? { ...settings, sampledDamageRolls: value } : { ...settings, damageRolls: value },
        )
      }
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
        label="Depth, exact"
        title="The turns of lookahead for Double Oracle, Serialized Bounds, Backward Induction, PIMC, and MCTS. Each ply multiplies the tree by the branch count of a turn, so a doubles position cannot finish depth 2."
        value={settings.depth}
        min={1}
        max={8}
        disabled={disabled}
        onChange={(value) => set('depth', value)}
      />
      <NumberField
        label="Depth, sampled"
        title="The turns of lookahead for ISMCTS and MCCFR. These searches draw one outcome for each ply, so one budget buys about the same seconds at every depth. It buys fewer and deeper iterations instead. A 30-second answer holds about 36,800 iterations at depth 1, 18,200 at depth 2, and 12,100 at depth 3."
        value={settings.sampledDepth}
        min={1}
        max={8}
        disabled={disabled}
        onChange={(value) => set('sampledDepth', value)}
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
          title={
            settings.depth > PRESET_DEPTH
              ? 'Solves depth 1 over every action, then raises only the cells that decide the answer to the chosen depth. A doubles position cannot finish a complete depth-2 search, and this does reach depth 2 on the cells that matter. The answer reports how many actions it verified.'
              : 'Refinement raises the cells of the support from depth 1 to a deeper one, so it needs an exact depth above 1. Raise the exact depth to use it.'
          }
        >
          <input
            type="checkbox"
            checked={settings.refine && settings.depth > PRESET_DEPTH}
            // Refinement solves depth 1 and then raises the cells of the
            // support. A request at depth 1 has nothing to raise, and the server
            // rejects the flag there. Do not offer a switch that cannot act.
            disabled={disabled || settings.depth <= PRESET_DEPTH}
            onChange={(event) => set('refine', event.target.checked)}
          />
          Refine to depth
        </label>
      )}
    </div>
  )
}
