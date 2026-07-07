// Versioned localStorage persistence for teams and formats.
// Bump a key's version and add a migration here if the schema ever changes.

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
  /** PokeAPI item names (slugs) that are banned in this format. */
  bannedItems: string[]
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
  return readJson<{ teams: StoredTeam[] }>(TEAMS_KEY)?.teams ?? []
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
  },
  {
    id: 'champions-s2-singles',
    name: 'Pokémon Champions Season 2 Singles',
    activePokemon: 1,
    totalPokemon: 6,
    broughtPokemon: 3,
    bannedItems: [],
  },
]

export function loadFormats(): StoredFormat[] {
  const stored = readJson<{ formats: StoredFormat[] }>(FORMATS_KEY)
  if (!stored) {
    saveFormats(DEFAULT_FORMATS)
    return DEFAULT_FORMATS
  }
  return stored.formats
}

export function saveFormats(formats: StoredFormat[]) {
  writeJson(FORMATS_KEY, { formats })
}

export function newId(): string {
  return crypto.randomUUID()
}

/** Last-used new-battle configuration, restored between games. */
export interface BattleSetup {
  formatId: string
  team1Id: string
  team2Id: string
}

const SETUP_KEY = 'pokerust.battleSetup.v1'

export function loadBattleSetup(): BattleSetup | null {
  return readJson<BattleSetup>(SETUP_KEY)
}

export function saveBattleSetup(setup: BattleSetup) {
  writeJson(SETUP_KEY, setup)
}
