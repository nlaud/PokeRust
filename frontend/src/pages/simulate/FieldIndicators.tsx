import type { BattleView } from '../../api/types'

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
  'Light Screen': 'bg-amber-300/70 text-amber-950',
  'Aurora Veil': 'bg-sky-300/70 text-sky-950',
  'Stealth Rock': 'bg-stone-500/70 text-white',
  Tailwind: 'bg-teal-500/70 text-white',
}

function chip(name: string, turns: number | undefined, key: string, prefix?: string) {
  const color = EFFECT_COLORS[name] ?? 'bg-slate-400/70 text-white'
  return (
    <span key={key} className={`rounded-md px-2.5 py-1 text-sm font-semibold shadow-sm backdrop-blur ${color}`}>
      {prefix ? `${prefix} ` : ''}
      {name}
      {turns !== undefined ? ` (${turns})` : ''}
    </span>
  )
}

export default function FieldIndicators({ view }: { view: BattleView }) {
  const field = view.field
  if (!field) return null

  const chips = [
    field.weather && chip(field.weather.name, field.weather.turns, 'weather'),
    field.terrain && chip(field.terrain.name, field.terrain.turns, 'terrain'),
    ...field.pseudoWeathers.map((pw, i) => chip(pw.name, pw.turns, `pw-${i}`)),
    ...(view.p1?.sideConditions ?? []).map((sc, i) => chip(sc.name, sc.turns, `p1-${i}`, 'P1')),
    ...(view.p2?.sideConditions ?? []).map((sc, i) => chip(sc.name, sc.turns, `p2-${i}`, 'P2')),
  ].filter(Boolean)

  if (chips.length === 0) return null
  return <div className="absolute left-3 top-3 z-10 flex max-w-80 flex-wrap gap-2">{chips}</div>
}
