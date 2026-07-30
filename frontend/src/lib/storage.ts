// Stores versioned teams and formats in local storage.
// After a schema change, increase the key version and add a migration.

export interface StoredTeam {
  id: string
  name: string
  /** Raw Showdown-export teamsheet text. */
  sheet: string
  favorite: boolean
  updatedAt: string
}

export interface StoredFormat {
  id: string
  name: string
  activePokemon: number
  totalPokemon: number
  broughtPokemon: number
  /** PokeAPI item slugs that this format bans. */
  bannedItems: string[]
  /** Sets all inferred opponent IVs to the Champions default of 31. */
  forceMaxIvs: boolean
  /** Once-per-battle mechanics permitted by this regulation. */
  teraEnabled: boolean
  megaEnabled: boolean
  favorite: boolean
}

const TEAMS_KEY = 'pokerust.teams.v1'
const FORMATS_KEY = 'pokerust.formats.v1'

function readJson<T>(key: string): T | null {
  try {
    const raw = localStorage.getItem(key)
    return raw ? (JSON.parse(raw) as T) : null
  } catch {
    return null
  }
}

function writeJson(key: string, value: unknown) {
  localStorage.setItem(key, JSON.stringify(value))
}

export function loadTeams(): StoredTeam[] {
  const teams = readJson<{ teams: StoredTeam[] }>(TEAMS_KEY)?.teams ?? []
  // Add `favorite` to old stored rows.
  return teams.map((t) => ({ ...t, favorite: t.favorite ?? false }))
}

export function saveTeams(teams: StoredTeam[]) {
  writeJson(TEAMS_KEY, { teams })
}

const DEFAULT_FORMATS: StoredFormat[] = [
  {
    id: 'champions-s2-doubles',
    name: 'Pokémon Champions Season 2 Doubles',
    activePokemon: 2,
    totalPokemon: 6,
    broughtPokemon: 4,
    bannedItems: [],
    forceMaxIvs: true,
    teraEnabled: true,
    megaEnabled: true,
    favorite: false,
  },
  {
    id: 'champions-s2-singles',
    name: 'Pokémon Champions Season 2 Singles',
    activePokemon: 1,
    totalPokemon: 6,
    broughtPokemon: 3,
    bannedItems: [],
    forceMaxIvs: true,
    teraEnabled: true,
    megaEnabled: true,
    favorite: false,
  },
]

export function loadFormats(): StoredFormat[] {
  const stored = readJson<{ formats: StoredFormat[] }>(FORMATS_KEY)
  if (!stored) {
    saveFormats(DEFAULT_FORMATS)
    return DEFAULT_FORMATS
  }
  // Add new fields to old stored rows.
  return stored.formats.map((f) => ({
    ...f,
    favorite: f.favorite ?? false,
    forceMaxIvs: f.forceMaxIvs ?? true,
    teraEnabled: f.teraEnabled ?? true,
    megaEnabled: f.megaEnabled ?? true,
  }))
}

export function saveFormats(formats: StoredFormat[]) {
  writeJson(FORMATS_KEY, { formats })
}

export function newId(): string {
  return crypto.randomUUID()
}

/** Sorts favorite items first. Keeps the order within each group. */
export function favoritesFirst<T extends { favorite: boolean }>(items: T[]): T[] {
  return [...items].sort((a, b) => Number(b.favorite) - Number(a.favorite))
}

/** Stores the latest new-battle configuration. */
export interface BattleSetup {
  formatId: string
  team1Id: string
  team2Id: string
  informationMode?: 'closedSheet' | 'perfect' | 'openSheet' | 'openSheetNatures'
  /** Selects a saved team or a generated team.
   * An absent value selects the saved team. */
  team1Source?: 'saved' | 'meta'
  team2Source?: 'saved' | 'meta'
}

const SETUP_KEY = 'pokerust.battleSetup.v1'

export function loadBattleSetup(): BattleSetup | null {
  return readJson<BattleSetup>(SETUP_KEY)
}

export function saveBattleSetup(setup: BattleSetup) {
  writeJson(SETUP_KEY, setup)
}
