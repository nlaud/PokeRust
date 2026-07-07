// Static item catalog for the Format editor: exactly the held-item pool of
// Pokémon Champions (general items, Mega Stones, berries). Slugs follow the
// PokeAPI sprites-repo naming so `itemSpriteUrl` resolves; Champions-only Mega
// Stones have no sprite there and render label-only (the <img> onError hides
// the broken image).

export interface CatalogItem {
  /** PokeAPI slug, e.g. "choice-scarf". */
  name: string
  /** Human label, e.g. "Choice Scarf". */
  label: string
}

/** "King's Rock" → "kings-rock"; "Charizardite X" → "charizardite-x". */
function slugify(label: string): string {
  return label
    .toLowerCase()
    .replace(/[.'’]/g, '')
    .replace(/[\s_]+/g, '-')
}

function items(labels: string[]): CatalogItem[] {
  return labels.map((label) => ({ name: slugify(label), label }))
}

const GENERAL = items([
  'Big Root', 'Black Belt', 'Black Glasses', 'Bright Powder', 'Charcoal',
  'Choice Scarf', 'Damp Rock', 'Dragon Fang', 'Expert Belt', 'Fairy Feather',
  'Focus Band', 'Focus Sash', 'Hard Stone', 'Heat Rock', 'Icy Rock',
  'Iron Ball', "King's Rock", 'Leftovers', 'Life Orb', 'Light Ball',
  'Light Clay', 'Magnet', 'Mental Herb', 'Metal Coat', 'Metronome',
  'Miracle Seed', 'Muscle Band', 'Mystic Water', 'Never-Melt Ice',
  'Poison Barb', 'Quick Claw', 'Scope Lens', 'Sharp Beak', 'Shed Shell',
  'Shell Bell', 'Silk Scarf', 'Silver Powder', 'Smooth Rock', 'Soft Sand',
  'Spell Tag', 'Twisted Spoon', 'White Herb', 'Wide Lens', 'Wise Glasses',
  'Zoom Lens',
])

const MEGA_STONES = items([
  'Abomasite', 'Absolite', 'Aerodactylite', 'Aggronite', 'Alakazite',
  'Altarianite', 'Ampharosite', 'Audinite', 'Banettite', 'Barbaracite',
  'Beedrillite', 'Blastoisinite', 'Blazikenite', 'Cameruptite', 'Chandelurite',
  'Charizardite X', 'Charizardite Y', 'Chesnaughtite', 'Chimechite',
  'Clefablite', 'Crabominite', 'Delphoxite', 'Dragalgite', 'Dragoninite',
  'Drampanite', 'Eelektrossite', 'Emboarite', 'Excadrite', 'Falinksite',
  'Feraligite', 'Floettite', 'Froslassite', 'Galladite', 'Garchompite',
  'Gardevoirite', 'Gengarite', 'Glalitite', 'Glimmoranite', 'Golurkite',
  'Greninjite', 'Gyaradosite', 'Hawluchanite', 'Heracronite', 'Houndoominite',
  'Kangaskhanite', 'Lopunnite', 'Lucarionite', 'Malamarite', 'Manectite',
  'Mawilite', 'Medichamite', 'Meganiumite', 'Meowsticite', 'Metagrossite',
  'Pidgeotite', 'Pinsirite', 'Pyroarite', 'Raichunite X', 'Raichunite Y',
  'Sablenite', 'Sceptilite', 'Scizorite', 'Scolipite', 'Scovillainite',
  'Scraftinite', 'Sharpedonite', 'Skarmorite', 'Slowbronite', 'Staraptite',
  'Starminite', 'Steelixite', 'Swampertite', 'Tyranitarite', 'Venusaurite',
  'Victreebelite',
])

const BERRIES = items(
  [
    'Aspear', 'Babiri', 'Charti', 'Cheri', 'Chesto', 'Chilan', 'Chople',
    'Coba', 'Colbur', 'Haban', 'Kasib', 'Kebia', 'Leppa', 'Lum', 'Occa',
    'Oran', 'Passho', 'Payapa', 'Pecha', 'Persim', 'Rawst', 'Rindo', 'Roseli',
    'Shuca', 'Sitrus', 'Tanga', 'Wacan', 'Yache',
  ].map((n) => `${n} Berry`),
)

const CATALOG: CatalogItem[] = [...GENERAL, ...MEGA_STONES, ...BERRIES]

export function fetchItemCatalog(): Promise<CatalogItem[]> {
  return Promise.resolve(CATALOG)
}
