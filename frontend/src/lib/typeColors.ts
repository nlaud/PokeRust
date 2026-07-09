// Canonical Pokémon type colors (Bulbapedia Gen VI+ palette), keyed by the
// exact strings the server emits — the Rust `PokemonType` enum's Debug name
// (see poke_rust/src/bin/server/mapping.rs, `types: mon.types...format!("{:?}", t)`).
// Values are hex, not Tailwind classes: Tailwind v4 purges any class string it
// can't see literally at build time, so these are applied via inline `style`.

const DARK_TEXT = '#1a1a1a'
const WHITE_TEXT = '#ffffff'

interface TypeColor {
  bg: string
  text: string
}

// A handful of canonical colors are pale enough that white text washes out,
// so those get dark text instead.
export const TYPE_COLORS: Record<string, TypeColor> = {
  Normal: { bg: '#A8A77A', text: WHITE_TEXT },
  Fire: { bg: '#EE8130', text: WHITE_TEXT },
  Water: { bg: '#6390F0', text: WHITE_TEXT },
  Electric: { bg: '#F7D02C', text: DARK_TEXT },
  Grass: { bg: '#7AC74C', text: WHITE_TEXT },
  Ice: { bg: '#96D9D6', text: DARK_TEXT },
  Fighting: { bg: '#C22E28', text: WHITE_TEXT },
  Poison: { bg: '#A33EA1', text: WHITE_TEXT },
  Ground: { bg: '#E2BF65', text: DARK_TEXT },
  Flying: { bg: '#A98FF3', text: WHITE_TEXT },
  Psychic: { bg: '#F95587', text: WHITE_TEXT },
  Bug: { bg: '#A6B91A', text: WHITE_TEXT },
  Rock: { bg: '#B6A136', text: WHITE_TEXT },
  Ghost: { bg: '#735797', text: WHITE_TEXT },
  Dragon: { bg: '#6F35FC', text: WHITE_TEXT },
  Dark: { bg: '#705746', text: WHITE_TEXT },
  Steel: { bg: '#B7B7CE', text: DARK_TEXT },
  Fairy: { bg: '#D685AD', text: WHITE_TEXT },
}

const FALLBACK: TypeColor = { bg: '#6b7280', text: WHITE_TEXT }

/** Inline-style object for a type badge, with a neutral grey fallback for unrecognized types. */
export function typeStyle(type: string): { backgroundColor: string; color: string } {
  const color = TYPE_COLORS[type] ?? FALLBACK
  return { backgroundColor: color.bg, color: color.text }
}
