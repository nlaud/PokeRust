import type { BattleView, NamedTurns } from '../../api/types'

/** Chip colors keyed by effect name; unlisted effects get the neutral chip. */
const EFFECT_COLORS: Record<string, string> = {
  Rain: 'bg-blue-500/70 text-white',
  'Heavy Rain': 'bg-blue-700/70 text-white',
  Sun: 'bg-amber-400/70 text-amber-950',
  'Extreme Sunlight': 'bg-amber-500/70 text-white',
  Sandstorm: 'bg-yellow-600/70 text-white',
  Snow: 'bg-cyan-300/70 text-cyan-950',
  'Strong Winds': 'bg-teal-400/70 text-teal-950',
  'Electric Terrain': 'bg-yellow-300/70 text-yellow-950',
  'Grassy Terrain': 'bg-green-400/70 text-green-950',
  'Misty Terrain': 'bg-rose-300/70 text-rose-950',
  'Psychic Terrain': 'bg-fuchsia-400/70 text-white',
  'Trick Room': 'bg-indigo-500/70 text-white',
  Gravity: 'bg-slate-500/70 text-white',
  Reflect: 'bg-violet-400/70 text-white',
  'Light Screen': 'bg-violet-400/70 text-white',
  'Aurora Veil': 'bg-sky-300/70 text-sky-950',
  'Stealth Rock': 'bg-stone-500/70 text-white',
  Tailwind: 'bg-teal-500/70 text-white',
}

/** "(5)" once collapsed to an exact value, "(5 or 8)" while fog-of-war still
 * leaves the effect's setter's item (an extension rock or not) unrevealed —
 * never narrower than the belief's actual candidate range, see
 * `NamedTurns`'s doc comment. Exactly two discrete possibilities, not a
 * continuous span, so "5 or 8" reads correctly where "5-8" would wrongly
 * imply every value in between is also possible. */
function turnsLabel(effect: NamedTurns): string {
  if (effect.turns === undefined) return ''
  if (effect.turnsMax !== undefined && effect.turnsMax !== effect.turns) {
    return ` (${effect.turns} or ${effect.turnsMax})`
  }
  return ` (${effect.turns})`
}

function chip(effect: NamedTurns, key: string, prefix?: string) {
  const color = EFFECT_COLORS[effect.name] ?? 'bg-slate-400/70 text-white'
  return (
    <span key={key} className={`rounded-md px-2.5 py-1 text-sm font-semibold shadow-sm backdrop-blur ${color}`}>
      {prefix ? `${prefix} ` : ''}
      {effect.name}
      {turnsLabel(effect)}
    </span>
  )
}

export default function FieldIndicators({ view }: { view: BattleView }) {
  const field = view.field
  if (!field) return null

  const chips = [
    field.weather && chip(field.weather, 'weather'),
    field.terrain && chip(field.terrain, 'terrain'),
    ...field.pseudoWeathers.map((pw, i) => chip(pw, `pw-${i}`)),
    ...(view.p1?.sideConditions ?? []).map((sc, i) => chip(sc, `p1-${i}`, 'P1')),
    ...(view.p2?.sideConditions ?? []).map((sc, i) => chip(sc, `p2-${i}`, 'P2')),
  ].filter(Boolean)

  if (chips.length === 0) return null
  return <div className="absolute left-3 top-3 z-10 flex max-w-80 flex-wrap gap-2">{chips}</div>
}
