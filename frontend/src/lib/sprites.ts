// Sprite resolution against PokeAPI. Nothing is ever bundled into the repo:
// species/item names resolve to raw.githubusercontent sprite URLs at runtime
// and the browser HTTP cache holds the images.

/**
 * Showdown display names whose PokeAPI slug does not follow the plain
 * lowercase/hyphenate rule. Extend this table when a sprite 404s.
 */
const SLUG_EXCEPTIONS: Record<string, string> = {
  'indeedee-f': 'indeedee-female',
  'meowstic-f': 'meowstic-female',
  'basculegion-f': 'basculegion-female',
  'oinkologne-f': 'oinkologne-female',
  urshifu: 'urshifu-single-strike',
  'urshifu-rapid-strike': 'urshifu-rapid-strike',
  zacian: 'zacian',
  zamazenta: 'zamazenta',
  eiscue: 'eiscue-ice',
  indeedee: 'indeedee-male',
  meowstic: 'meowstic-male',
  basculin: 'basculin-red-striped',
  morpeko: 'morpeko-full-belly',
  wishiwashi: 'wishiwashi-solo',
  toxtricity: 'toxtricity-amped',
  lycanroc: 'lycanroc-midday',
  oricorio: 'oricorio-baile',
  minior: 'minior-red-meteor',
  mimikyu: 'mimikyu-disguised',
  darmanitan: 'darmanitan-standard',
  'darmanitan-galar': 'darmanitan-galar-standard',
  giratina: 'giratina-altered',
  shaymin: 'shaymin-land',
  wormadam: 'wormadam-plant',
  deoxys: 'deoxys-normal',
  keldeo: 'keldeo-ordinary',
  meloetta: 'meloetta-aria',
  thundurus: 'thundurus-incarnate',
  tornadus: 'tornadus-incarnate',
  landorus: 'landorus-incarnate',
  enamorus: 'enamorus-incarnate',
  aegislash: 'aegislash-shield',
  pumpkaboo: 'pumpkaboo-average',
  gourgeist: 'gourgeist-average',
  zygarde: 'zygarde-50',
  'tauros-paldea-combat': 'tauros-paldea-combat-breed',
  'tauros-paldea-blaze': 'tauros-paldea-blaze-breed',
  'tauros-paldea-aqua': 'tauros-paldea-aqua-breed',
  'maushold-four': 'maushold-family-of-four',
  maushold: 'maushold-family-of-three',
  squawkabilly: 'squawkabilly-green-plumage',
  palafin: 'palafin-zero',
  tatsugiri: 'tatsugiri-curly',
  dudunsparce: 'dudunsparce-two-segment',
  'ogerpon-wellspring': 'ogerpon-wellspring-mask',
  'ogerpon-hearthflame': 'ogerpon-hearthflame-mask',
  'ogerpon-cornerstone': 'ogerpon-cornerstone-mask',
}

/** Convert a Showdown display name ("Abomasnow-Mega") to a PokeAPI slug. */
export function showdownToSlug(species: string): string {
  const plain = species
    .toLowerCase()
    .replace(/[.':’%]/g, '')
    .replace(/[\s_]+/g, '-')
  return SLUG_EXCEPTIONS[plain] ?? plain
}

export interface SpriteUrls {
  front: string | null
  back: string | null
}

const memoryCache = new Map<string, Promise<SpriteUrls>>()
const CACHE_KEY = 'pokerust.spriteCache.v1'

function readPersistentCache(): Record<string, SpriteUrls> {
  try {
    return JSON.parse(localStorage.getItem(CACHE_KEY) ?? '{}')
  } catch {
    return {}
  }
}

function persist(slug: string, urls: SpriteUrls) {
  const cache = readPersistentCache()
  cache[slug] = urls
  try {
    localStorage.setItem(CACHE_KEY, JSON.stringify(cache))
  } catch {
    // Storage full — the in-memory cache still works for this session.
  }
}

export function fetchSprites(species: string): Promise<SpriteUrls> {
  const slug = showdownToSlug(species)

  const cached = memoryCache.get(slug)
  if (cached) return cached

  const persisted = readPersistentCache()[slug]
  if (persisted) {
    const resolved = Promise.resolve(persisted)
    memoryCache.set(slug, resolved)
    return resolved
  }

  const promise = (async (): Promise<SpriteUrls> => {
    const response = await fetch(`https://pokeapi.co/api/v2/pokemon/${slug}`)
    if (!response.ok) {
      console.warn(`PokeAPI sprite lookup failed for "${species}" (slug "${slug}")`)
      return { front: null, back: null }
    }
    const data = await response.json()
    const urls: SpriteUrls = {
      front: data.sprites?.front_default ?? null,
      back: data.sprites?.back_default ?? null,
    }
    persist(slug, urls)
    return urls
  })()

  memoryCache.set(slug, promise)
  return promise
}

/** Item sprites live at a predictable URL in the PokeAPI sprites repo. */
export function itemSpriteUrl(itemSlug: string): string {
  return `https://raw.githubusercontent.com/PokeAPI/sprites/master/sprites/items/${itemSlug}.png`
}
