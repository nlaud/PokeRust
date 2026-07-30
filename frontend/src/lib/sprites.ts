// Resolves species and item sprites through PokeAPI.
// The repository does not contain sprite files.
// The browser stores downloaded images in its HTTP cache.

/** Maps Showdown names that need a special PokeAPI slug.
 * Add an entry when the normal slug returns HTTP 404. */
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
  // PokeAPI provides only gendered Basculegion forms.
  basculegion: 'basculegion-male',
  'basculegion-m': 'basculegion-male',
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
  // Read and write the cache without an `await`.
  // Thus, another `persist` call cannot change it between these operations.
  const cache = readPersistentCache()
  cache[slug] = urls
  try {
    localStorage.setItem(CACHE_KEY, JSON.stringify(cache))
  } catch {
    // Continue with the memory cache when local storage is full.
  }
}

/** Limits concurrent PokeAPI requests to prevent dropped sprite requests. */
const MAX_CONCURRENT_REQUESTS = 5

function createLimiter(max: number) {
  let active = 0
  const queue: (() => void)[] = []

  function schedule() {
    if (active >= max || queue.length === 0) return
    active++
    const run = queue.shift()!
    run()
  }

  return function limit<T>(task: () => Promise<T>): Promise<T> {
    return new Promise((resolve, reject) => {
      queue.push(() => {
        task()
          .then(resolve, reject)
          .finally(() => {
            active--
            schedule()
          })
      })
      schedule()
    })
  }
}

const limit = createLimiter(MAX_CONCURRENT_REQUESTS)

const REQUEST_TIMEOUT_MS = 8000
const MAX_FETCH_RETRIES = 3
const RETRY_BASE_DELAY_MS = 300

function delay(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms))
}

/** Fetches a PokeAPI URL through the request limiter.
 * Retries temporary failures with a backoff.
 * Does not retry HTTP 404.
 * Throws after the final temporary failure. */
function pokeApiFetch(url: string): Promise<Response> {
  return limit(async () => {
    let lastError: unknown
    for (let attempt = 0; attempt <= MAX_FETCH_RETRIES; attempt++) {
      const controller = new AbortController()
      const timer = setTimeout(() => controller.abort(), REQUEST_TIMEOUT_MS)
      try {
        const response = await fetch(url, { signal: controller.signal })
        if (response.ok || response.status === 404) return response
        lastError = new Error(`PokeAPI ${response.status} for ${url}`)
      } catch (err) {
        lastError = err
      } finally {
        clearTimeout(timer)
      }
      if (attempt < MAX_FETCH_RETRIES) {
        const backoff = RETRY_BASE_DELAY_MS * 2 ** attempt
        await delay(backoff + Math.random() * backoff * 0.5)
      }
    }
    throw lastError
  })
}

export function fetchSprites(species: string): Promise<SpriteUrls> {
  const slug = showdownToSlug(species)

  const cached = memoryCache.get(slug)
  if (cached) return cached

  // A stored null pair records a confirmed missing sprite.
  // Return it without another network request.
  const persisted = readPersistentCache()[slug]
  if (persisted) {
    const resolved = Promise.resolve(persisted)
    memoryCache.set(slug, resolved)
    return resolved
  }

  const promise = (async (): Promise<SpriteUrls> => {
    try {
      const urls = await resolveSprites(slug)
      if (urls) {
        persist(slug, urls)
        return urls
      }
      // Each candidate returned HTTP 404.
      // Store the null result to prevent later network requests.
      console.warn(`PokeAPI sprite lookup failed for "${species}" (slug "${slug}")`)
      persist(slug, { front: null, back: null })
      return { front: null, back: null }
    } catch (err) {
      // Do not cache a temporary network failure.
      // Remove the pending request so that the next caller can retry.
      memoryCache.delete(slug)
      throw err
    }
  })()

  memoryCache.set(slug, promise)
  return promise
}

/** Gets the default sprite URLs for one slug.
 * Returns `null` after HTTP 404. */
async function spritesFromPokemonEndpoint(slug: string): Promise<SpriteUrls | null> {
  const response = await pokeApiFetch(`https://pokeapi.co/api/v2/pokemon/${slug}`)
  if (!response.ok) return null
  const data = await response.json()
  return {
    front: data.sprites?.front_default ?? null,
    back: data.sprites?.back_default ?? null,
  }
}

/** Resolves a sprite slug with PokeAPI fallbacks.
 * First, checks the Pokémon endpoint.
 * Next, checks the default species variety.
 * Last, removes trailing form tokens one at a time.
 * Propagates temporary failures. */
async function resolveSprites(slug: string): Promise<SpriteUrls | null> {
  let candidate = slug
  for (;;) {
    const direct = await spritesFromPokemonEndpoint(candidate)
    if (direct) return direct

    const speciesResponse = await pokeApiFetch(`https://pokeapi.co/api/v2/pokemon-species/${candidate}`)
    if (speciesResponse.ok) {
      const data = await speciesResponse.json()
      const varieties = data.varieties as { is_default: boolean; pokemon: { name: string } }[]
      const defaultName = varieties?.find((v) => v.is_default)?.pokemon.name
      if (defaultName && defaultName !== candidate) {
        const viaSpecies = await spritesFromPokemonEndpoint(defaultName)
        if (viaSpecies) return viaSpecies
      }
    }

    const lastHyphen = candidate.lastIndexOf('-')
    if (lastHyphen <= 0) return null
    candidate = candidate.slice(0, lastHyphen)
  }
}

/** Item sprites live at a predictable URL in the PokeAPI sprites repo. */
export function itemSpriteUrl(itemSlug: string): string {
  return `https://raw.githubusercontent.com/PokeAPI/sprites/master/sprites/items/${itemSlug}.png`
}

/** Routes a GitHub sprite URL through the server disk cache.
 * The URL caches still use the original external URL as their key. */
export function cachedImageUrl(url: string): string {
  return `/api/sprites?url=${encodeURIComponent(url)}`
}

/** Derives possible Mega form names from the species and held item. */
export function megaFormeNames(species: string, item: string | null | undefined): string[] {
  if (!item) return []
  const xy = item.match(/^(.+)ite ([XY])$/)
  if (xy && species.toLowerCase().startsWith(xy[1].toLowerCase().slice(0, 4))) {
    return [`${species}-Mega-${xy[2]}`]
  }
  const plain = item.match(/^(.+)ite$/)
  if (plain && species.toLowerCase().startsWith(plain[1].toLowerCase().slice(0, 4))) {
    return [`${species}-Mega`]
  }
  return []
}

/** Loads species sprites into the browser cache.
 * Repeated calls use the existing URL and browser caches. */
export function preloadSprites(speciesList: string[]) {
  for (const species of new Set(speciesList)) {
    if (!species) continue
    void fetchSprites(species)
      .then((urls) => {
        for (const url of [urls.front, urls.back]) {
          if (url) new Image().src = cachedImageUrl(url)
        }
      })
      .catch(() => {
        // Let the mounted Sprite component or a later preload retry this failure.
      })
  }
}
