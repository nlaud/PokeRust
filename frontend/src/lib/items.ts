// Item catalog for the Format editor, fetched once from PokeAPI and cached in
// localStorage. Item names here are PokeAPI slugs (e.g. "choice-scarf").

const CATALOG_KEY = 'pokerust.itemCatalog.v1'

export interface CatalogItem {
  /** PokeAPI slug, e.g. "choice-scarf". */
  name: string
  /** Human label, e.g. "Choice Scarf". */
  label: string
}

function slugToLabel(slug: string): string {
  return slug
    .split('-')
    .map((word) => word.charAt(0).toUpperCase() + word.slice(1))
    .join(' ')
}

let inFlight: Promise<CatalogItem[]> | null = null

export function fetchItemCatalog(): Promise<CatalogItem[]> {
  const cached = localStorage.getItem(CATALOG_KEY)
  if (cached) {
    try {
      return Promise.resolve(JSON.parse(cached) as CatalogItem[])
    } catch {
      localStorage.removeItem(CATALOG_KEY)
    }
  }
  if (inFlight) return inFlight

  inFlight = (async () => {
    const response = await fetch('https://pokeapi.co/api/v2/item?limit=2200')
    if (!response.ok) throw new Error('Failed to fetch item catalog from PokeAPI')
    const data = await response.json()
    const items: CatalogItem[] = (data.results as { name: string }[]).map((item) => ({
      name: item.name,
      label: slugToLabel(item.name),
    }))
    try {
      localStorage.setItem(CATALOG_KEY, JSON.stringify(items))
    } catch {
      // Storage full — refetch next session.
    }
    return items
  })()
  return inFlight
}
