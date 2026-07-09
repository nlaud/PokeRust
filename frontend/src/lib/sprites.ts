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
  // PokeAPI has no plain "basculegion" pokemon endpoint — only gendered forms.
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
  // Read-modify-write, but synchronous end-to-end (no `await` between the
  // read and the write) — JS's single-threaded execution means this can't
  // interleave with another `persist()` call, even though many sprite
  // resolutions are in flight concurrently.
  const cache = readPersistentCache()
  cache[slug] = urls
  try {
    localStorage.setItem(CACHE_KEY, JSON.stringify(cache))
  } catch {
    // Storage full — the in-memory cache still works for this session.
  }
}

/** Requests in flight against pokeapi.co at once. Keeps a cold Teams-page
 *  load (dozens of sprites mounting at once) from bursting past what PokeAPI
 *  will tolerate — the burst was the main cause of dropped sprites. */
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

/**
 * Fetch a PokeAPI URL through the concurrency limiter, with a timeout and
 * retry-with-backoff on transient failures (network error, timeout, 429,
 * 5xx). A 404 is a definitive "not found" — it's returned as a normal
 * (non-ok) Response, not retried. Throws only once every retry on a
 * transient failure has been exhausted; callers treat that throw as "try
 * again later," never as "this sprite doesn't exist."
 */
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

  // A stored `{front: null, back: null}` is a truthy object, so a
  // previously-confirmed "this sprite doesn't exist" result is served from
  // cache here too, instead of re-running the whole resolution chain.
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
      // Every candidate in the fallback chain came back a clean 404 — this
      // sprite genuinely doesn't exist. Persist the null result so future
      // loads don't pay the resolution chain's network cost again.
      console.warn(`PokeAPI sprite lookup failed for "${species}" (slug "${slug}")`)
      persist(slug, { front: null, back: null })
      return { front: null, back: null }
    } catch (err) {
      // Transient failure (network error/timeout/5xx surviving every
      // retry) — never cache this outcome. Evict the in-flight promise so
      // the next caller (e.g. Sprite's own retry effect) re-attempts the
      // network instead of being stuck behind a memoized rejection.
      memoryCache.delete(slug)
      throw err
    }
  })()

  memoryCache.set(slug, promise)
  return promise
}

/** GET /pokemon/{slug} and pull out the default sprite URLs, or null on 404. */
async function spritesFromPokemonEndpoint(slug: string): Promise<SpriteUrls | null> {
  const response = await pokeApiFetch(`https://pokeapi.co/api/v2/pokemon/${slug}`)
  if (!response.ok) return null
  const data = await response.json()
  return {
    front: data.sprites?.front_default ?? null,
    back: data.sprites?.back_default ?? null,
  }
}

/**
 * Resolve a slug against PokeAPI with fallbacks for formes it doesn't know:
 * 1. the pokemon endpoint directly;
 * 2. the species endpoint's default variety (e.g. "basculegion" →
 *    "basculegion-male");
 * 3. progressively strip trailing hyphen tokens (Champions-only megas like
 *    "chandelure-mega" fall back to the base "chandelure" sprite), retrying
 *    1–2 each time.
 *
 * `pokeApiFetch` already retries transient failures internally; if it still
 * throws, that error propagates straight out of this function (and out to
 * `fetchSprites`) rather than being swallowed into "try the next candidate."
 * Otherwise a real outage would silently walk the whole fallback chain and
 * land on a false "doesn't exist" — exactly the permanent-placeholder bug
 * this rework fixes.
 */
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

/**
 * Mega forme display names a mon could turn into, derived from its held item:
 * "Charizardite X" → "Charizard-Mega-X"; any other "…ite" stone whose stem
 * prefixes the species name ("Tyranitarite" → "Tyranitar-Mega"). Champions-only
 * megas resolve through the 404 fallback chain to the base sprite — the same
 * URL the in-battle mega display will use, so preloading it is still a win.
 */
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

/**
 * Resolve and warm the browser image cache for a batch of species in the
 * background, so battle sprites render without loading placeholders. Safe to
 * call repeatedly: URL lookups hit the memory/localStorage caches and the
 * browser dedupes the image fetches.
 */
export function preloadSprites(speciesList: string[]) {
  for (const species of new Set(speciesList)) {
    if (!species) continue
    void fetchSprites(species)
      .then((urls) => {
        for (const url of [urls.front, urls.back]) {
          if (url) new Image().src = url
        }
      })
      .catch(() => {
        // Transient failure — not our job to retry here. A mounted
        // Sprite component (or a later preload pass) will pick it up.
      })
  }
}
