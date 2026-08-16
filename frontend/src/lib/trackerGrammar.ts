// Completes each word in the tracker input.
// This module mirrors the main grammar in `tracker_parse.rs`.
// It ranks the next tokens but does not create an `InformationEvent`.
// The server parser remains authoritative and returns HTTP 422 for invalid input.
// Keep the keyword tables and parser order synchronized with the Rust parser.
// Also keep `api/types.ts` synchronized with the Rust DTOs.

/** Converts text to the same lowercase alphanumeric form as the Rust parser. */
export function norm(s: string): string {
  return s
    .toLowerCase()
    .split('')
    .filter((c) => /[a-z0-9]/.test(c))
    .join('')
}

/** Checks tokens that need no word completion.
 * These tokens include HP values and hit counts. */
export function isSelfCompleteToken(partial: string): boolean {
  return /^\d+%?$/.test(partial) || /^\d+\/\d+$/.test(partial) || /^\d+hits?$/i.test(partial)
}

/** Stores the names that autocomplete can suggest.
 * The server supplies match species, moves, and abilities.
 * The item catalog supplies all items. */
export interface CompletionPools {
  species: string[]
  moves: string[]
  abilities: string[]
  items: string[]
}

export const EMPTY_POOLS: CompletionPools = { species: [], moves: [], abilities: [], items: [] }

// ── Casing detection ─────────────────────────────────────────────────────────
// Pools provide labels with spaces, such as "Rock Slide."
// The input separates tokens at spaces.
// A multiword name must therefore use Pascal, snake, kebab, or camel case.
// The Rust parser and `norm` treat these forms as equal.

export type CasingStyle = 'pascal' | 'snake' | 'kebab' | 'camel'

/** Finds the case style of one token.
 * Returns `null` when the token does not show a clear style. */
function detectCasing(raw: string): CasingStyle | null {
  if (raw.includes('_')) return 'snake'
  if (raw.includes('-')) return 'kebab'
  if (/[A-Z]/.test(raw.slice(1))) return /^[A-Z]/.test(raw) ? 'pascal' : 'camel'
  return null
}

/** Selects the case style for multiword completions.
 * Uses the latest clear style on the same line.
 * Uses Pascal case when the line has no clear style. */
export function styleFromTokens(tokens: string[], pools: CompletionPools): CasingStyle {
  const multiWordLabels = [...pools.species, ...pools.moves, ...pools.abilities, ...pools.items].filter((label) =>
    /\s/.test(label),
  )
  for (let i = tokens.length - 1; i >= 0; i--) {
    const raw = tokens[i]
    const n = norm(raw)
    if (multiWordLabels.some((label) => norm(label) === n)) {
      const style = detectCasing(raw)
      if (style) return style
    }
  }
  return 'pascal'
}

/** Converts a label to a supported case style without spaces. */
export function recase(label: string, style: CasingStyle): string {
  const words = label.split(/\s+/).filter((w) => w.length > 0)
  if (words.length === 0) return label
  switch (style) {
    case 'snake':
      return words.map((w) => w.toLowerCase()).join('_')
    case 'kebab':
      return words.map((w) => w.toLowerCase()).join('-')
    case 'camel':
      return words
        .map((w, i) => (i === 0 ? w.toLowerCase() : w[0].toUpperCase() + w.slice(1).toLowerCase()))
        .join('')
    case 'pascal':
      return words.map((w) => w[0].toUpperCase() + w.slice(1).toLowerCase()).join('')
  }
}

function recasePools(pools: CompletionPools, style: CasingStyle): CompletionPools {
  return {
    species: pools.species.map((s) => recase(s, style)),
    moves: pools.moves.map((s) => recase(s, style)),
    abilities: pools.abilities.map((s) => recase(s, style)),
    items: pools.items.map((s) => recase(s, style)),
  }
}

// ── Fixed keyword tables, mirroring tracker_parse.rs ────────────────────────
// Each concept lists all words that the Rust parser accepts.
// `canonical` is the short word that this module suggests.
// The server also accepts each listed alias.

interface WordGroup {
  canonical: string
  aliases: string[]
}

function group(canonical: string, ...extraAliases: string[]): WordGroup {
  return { canonical, aliases: [canonical, ...extraAliases] }
}

const STATUS_WORDS: WordGroup[] = [
  group('brn', 'burn', 'burned'),
  group('psn', 'poison', 'poisoned'),
  group('tox', 'badpoison', 'badlypoisoned', 'toxic'),
  group('par', 'para', 'paralyzed', 'paralysis', 'paralysed'),
  group('slp', 'sleep', 'asleep'),
  group('frz', 'frozen', 'freeze'),
]

const CANT_REASON_WORDS: WordGroup[] = [
  group('flinch', 'flinched'),
  group('fullpara', 'fullyparalyzed', 'fullparalysis', 'fullyparalysed'),
  group('sleep', 'asleep', 'slp'),
  group('frozen', 'frz', 'freeze'),
  group('recharge', 'mustrecharge', 'recharging'),
  group('taunt', 'taunted'),
  group('disable', 'disabled'),
  group('confusion', 'confused'),
  group('imprison', 'imprisoned'),
  group('attract', 'infatuated', 'infatuation'),
  group('bound', 'trapped'),
  group('throatchop', 'throatchopped'),
  group('torment', 'tormented'),
  group('focuspunch'),
  group('gravity'),
  group('healblock'),
  group('encore', 'encored'),
  group('skydrop'),
  group('beakblast'),
]

// Lists simple volatile names.
// Encore, Disable, and Stockpile use separate keywords.
const VOLATILE_WORDS: WordGroup[] = [
  group('confusion', 'confused'),
  group('leechseed', 'seeded'),
  group('taunt', 'taunted'),
  group('flashfire'),
  group('focusenergy'),
  group('aquaring'),
  group('attract', 'infatuated'),
  group('curse', 'cursed'),
  group('torment', 'tormented'),
  group('yawn'),
  group('saltcure'),
  group('tarshot'),
  group('minimize', 'minimized'),
  group('ingrain'),
  group('magnetrise'),
  group('protect', 'protected'),
  group('endure', 'enduring'),
  group('kingsshield'),
  group('banefulbunker'),
  group('spikyshield'),
  group('silktrap'),
  group('obstruct'),
  group('burningbulwark'),
  group('destinybond'),
  group('grudge'),
  group('embargo'),
  group('healblock'),
  group('imprison'),
  group('electrify'),
  group('powder'),
  group('syrupbomb'),
  group('telekinesis'),
  group('smackdown'),
  group('uproar'),
  group('roost'),
  group('rage'),
  group('ragepowder'),
  group('followme'),
  group('magiccoat'),
  group('snatch'),
  group('laserfocus'),
  group('miracleeye'),
  group('foresight'),
  group('octolock'),
  group('noretreat'),
  group('gastroacid'),
  group('sparklingaria'),
  group('glaiverush'),
  group('charge', 'charged'),
  group('defensecurl', 'defensecurled'),
  group('helpinghand'),
  group('powertrick'),
  group('forestscurse'),
  group('throatchop', 'throatchopped'),
  group('mustrecharge', 'recharging'),
  group('substitute', 'sub'),
  group('encore', 'encored'),
  group('disable', 'disabled'),
]

const WEATHER_WORDS: WordGroup[] = [
  group('rain', 'raindance', 'drizzle'),
  group('heavyrain', 'primordialsea'),
  group('sand', 'sandstorm'),
  group('snow', 'hail'),
  group('sun', 'sunnyday', 'sunny', 'drought'),
  group('extremesun', 'desolateland', 'harshsunlight'),
  group('strongwinds', 'deltastream'),
  group('none', 'clear'),
]

const TERRAIN_WORDS: WordGroup[] = [
  group('electric', 'electricterrain'),
  group('grassy', 'grassyterrain'),
  group('misty', 'mistyterrain'),
  group('psychic', 'psychicterrain'),
  group('none', 'clear'),
]

const ITEM_VERB_WORDS: WordGroup[] = [
  group('loses', 'lost', 'knockedoff'),
  group('consumes', 'consumed', 'ate', 'eats', 'used'),
  group('gains', 'gained', 'tricked', 'switcheroo', 'recycles'),
]

const STAT_WORDS: WordGroup[] = [
  group('atk', 'attack'),
  group('def', 'defense', 'defence'),
  group('spa', 'spatk', 'spattack', 'specialattack'),
  group('spd', 'spdef', 'spdefense', 'specialdefense'),
  group('spe', 'speed'),
  group('acc', 'accuracy'),
  group('eva', 'evasion', 'evasiveness'),
]

const MOVE_EFFECT_WORDS: WordGroup[] = [
  group('crit'),
  group('miss', 'missed'),
  group('immune'),
  group('blocked', 'block'),
  group('fail', 'failed'),
  // Marks the charge turn of this line's two-turn move.
  // The marker does not require a move argument.
  group('charging'),
  group('illusion', 'illusionended'),
  group('damage', 'damaged'),
  group('heal', 'healed'),
  group('sethp'),
  group('status', 'statusinflicted'),
  group('cure', 'statuscured'),
  group('volatileend', 'endvolatile'),
  group('encoremove'),
  group('disablemove'),
  group('stockpilelevel'),
  group('copyboosts', 'boostscopied'),
  group('invertboosts', 'boostsinverted'),
  group('field', 'pseudoweather'),
  group('side'),
  group('2hits'),
]

const SLOT_VERB_WORDS: WordGroup[] = [
  group('switch', 'switchin', 'sendout'),
  group('mega', 'megaevolve', 'megaevolution'),
  group('tera', 'terastallize', 'terastallized'),
  group('mustrecharge'),
  group('pass'),
  group('hp'),
  group('illusion', 'illusionended'),
  group('damage', 'damaged'),
  group('heal', 'healed'),
  group('sethp'),
  group('status', 'statusinflicted'),
  group('cure', 'statuscured'),
  group('volatileend', 'endvolatile'),
  group('encoremove'),
  group('disablemove'),
  group('stockpilelevel'),
  group('copyboosts', 'boostscopied'),
  group('invertboosts', 'boostsinverted'),
  // Accepts `o1 charging <move>` as an alias for the standard move line.
  group('charging'),
]

// Includes the 18 damage types and the Tera-only Stellar type.
const TYPE_WORDS: WordGroup[] = [
  'Normal', 'Fire', 'Water', 'Electric', 'Grass', 'Ice', 'Fighting', 'Poison',
  'Ground', 'Flying', 'Psychic', 'Bug', 'Rock', 'Ghost', 'Dragon', 'Dark',
  'Steel', 'Fairy', 'Stellar',
].map((t) => group(t))

const SLOT_WORDS: WordGroup[] = [group('p'), group('p1'), group('p2'), group('o'), group('o1'), group('o2')]
const END_OF_TURN_WORDS: WordGroup[] = [group('endofturn', 'eot')]
const FIELD_LINE_WORDS: WordGroup[] = [
  group('weather'),
  group('terrain'),
  group('field', 'pseudoweather'),
  group('side'),
]
const PSEUDO_WEATHER_WORDS: WordGroup[] = [
  group('fairylock'), group('gravity'), group('iondeluge'), group('magicdeluge'),
  group('mudsport'), group('trickroom'), group('watersport'), group('wonderroom'),
]
const SIDE_CONDITION_WORDS: WordGroup[] = [
  group('auroraveil'), group('reflect'), group('craftyshield'), group('lightscreen'),
  group('luckychant'), group('matblock'), group('mist'), group('quickguard'),
  group('safeguard'), group('spikes'), group('stealthrock'), group('stickyweb'),
  group('tailwind'), group('toxicspikes'), group('wideguard'),
]
const EFFECT_STATE_WORDS: WordGroup[] = [
  group('start', 'started', 'on'),
  group('end', 'ended', 'off'),
]
const SLOT_TARGET_WORDS = ['p1', 'p2', 'o1', 'o2']
const EXPLICIT_SLOT_WORDS = ['@p1', '@p2', '@o1', '@o2']
// `leads` starts a line and applies to a full side.
// `LEADS_SIDE_WORDS` lists its permitted side markers.
// These markers do not use slot digits.
const LEADS_LINE_WORDS: WordGroup[] = [group('leads')]
const LEADS_SIDE_WORDS: WordGroup[] = [group('p'), group('o')]
// `back` names the Pokemon that Player 1 brought but did not lead.
// The opponent's bring is hidden, so only the `p` side accepts the word.
const LEADS_BACK_WORDS: WordGroup[] = [group('back')]

function canonicalsOf(groups: WordGroup[]): string[] {
  return groups.map((g) => g.canonical)
}

// ── Levenshtein autocorrect fallback ────────────────────────────────────────

/** Calculates the edit distance between two strings.
 * The species picker also uses this function for spelling corrections. */
export function levenshtein(a: string, b: string): number {
  const dp: number[] = Array.from({ length: b.length + 1 }, (_, j) => j)
  for (let i = 1; i <= a.length; i++) {
    let prevDiag = dp[0]
    dp[0] = i
    for (let j = 1; j <= b.length; j++) {
      const temp = dp[j]
      dp[j] =
        a[i - 1] === b[j - 1] ? prevDiag : 1 + Math.min(prevDiag, dp[j], dp[j - 1])
      prevDiag = temp
    }
  }
  return dp[b.length]
}

/** Calculates a stable FNV-1a hash.
 * The hash gives suggestions a stable order that is not alphabetical. */
function stableHash(s: string): number {
  let h = 2166136261
  for (let i = 0; i < s.length; i++) {
    h ^= s.charCodeAt(i)
    h = Math.imul(h, 16777619)
  }
  return h >>> 0
}

function byStableHash(candidates: string[]): string[] {
  return [...candidates].sort((a, b) => stableHash(norm(a)) - stableHash(norm(b)))
}

/** Ranks prefix matches by stable hash.
 * If no prefix matches, ranks all candidates by edit distance. */
function rank(candidates: string[], partial: string): string[] {
  const p = norm(partial)
  if (p === '') return byStableHash(candidates)
  const prefixMatches = candidates.filter((c) => norm(c).startsWith(p))
  if (prefixMatches.length > 0) return byStableHash(prefixMatches)
  return [...candidates].sort((a, b) => {
    const d = levenshtein(norm(a), p) - levenshtein(norm(b), p)
    return d !== 0 ? d : stableHash(norm(a)) - stableHash(norm(b))
  })
}

// ── Script (de)serialization ─────────────────────────────────────────────────

/** Returns the content lines from one turn.
 * Removes the final end-of-turn line and adjacent blank lines. */
export function contentLinesOf(turnText: string): string[] {
  const lines = turnText.split('\n')
  while (lines.length > 0 && lines[lines.length - 1].trim() === '') lines.pop()
  if (lines.length > 0) {
    const last = norm(lines[lines.length - 1])
    if (last === 'endofturn' || last === 'eot') lines.pop()
  }
  return lines
}

/** Splits tracker text into turns.
 * Keeps incomplete text as the final turn.
 * The server remains the authority for valid text. */
export function splitScriptIntoTurns(script: string): string[] {
  if (script.trim() === '') return []
  const turns: string[] = []
  let current: string[] = []
  for (const line of script.split('\n')) {
    current.push(line)
    const trimmed = norm(line.trim())
    if (trimmed === 'endofturn' || trimmed === 'eot') {
      turns.push(current.join('\n'))
      current = []
    }
  }
  if (current.length > 0 && current.some((l) => l.trim() !== '')) {
    turns.push(current.join('\n'))
  }
  return turns
}

// ── Position classification ─────────────────────────────────────────────────

export type LinePosition =
  | { kind: 'lineStart' }
  | { kind: 'action' }
  | { kind: 'weatherWord' }
  | { kind: 'terrainWord' }
  | { kind: 'pseudoWeatherWord' }
  | { kind: 'effectState' }
  | { kind: 'sideMarker' }
  | { kind: 'sideConditionWord' }
  | { kind: 'statusWord' }
  | { kind: 'volatileWord' }
  | { kind: 'moveName' }
  | { kind: 'slotSource' }
  | { kind: 'stockpileLevel' }
  | { kind: 'switchSpecies' }
  | { kind: 'leadsSideOrSpecies' }
  | { kind: 'leadsSideSpeciesOrBack' }
  | { kind: 'megaSpeciesOrDone' }
  | { kind: 'teraType' }
  | { kind: 'itemVerbItem' }
  | { kind: 'chargingMove' }
  | { kind: 'moveBody' }
  | { kind: 'done' }

/** Reports whether the cursor can start the `back` clause of a `leads` line.
 * The clause belongs to the `p` side, and one line holds one clause. */
function leadsOffersBack(tokens: string[], cursorIndex: number): boolean {
  let onPlayerSide = false
  for (let i = 1; i < cursorIndex; i++) {
    const word = norm(tokens[i] ?? '')
    if (word === 'back') return false
    if (word === 'p' || word === 'p1' || word === 'player') onPlayerSide = true
    else if (word === 'o' || word === 'o1' || word === 'opponent') onPlayerSide = false
  }
  return onPlayerSide
}

/** Classifies the token at `cursorIndex` from the earlier tokens.
 * This order follows the Rust parser.
 * The server performs all validation. */
export function classifyPosition(tokens: string[], cursorIndex: number): LinePosition {
  if (cursorIndex === 0) return { kind: 'lineStart' }

  const first = norm(tokens[0] ?? '')
  if (first === 'weather') return cursorIndex === 1 ? { kind: 'weatherWord' } : { kind: 'done' }
  if (first === 'terrain') return cursorIndex === 1 ? { kind: 'terrainWord' } : { kind: 'done' }
  if (first === 'field' || first === 'pseudoweather') {
    if (cursorIndex === 1) return { kind: 'pseudoWeatherWord' }
    return cursorIndex === 2 ? { kind: 'effectState' } : { kind: 'done' }
  }
  if (first === 'side') {
    if (cursorIndex === 1) return { kind: 'sideMarker' }
    if (cursorIndex === 2) return { kind: 'sideConditionWord' }
    return cursorIndex === 3 ? { kind: 'effectState' } : { kind: 'done' }
  }
  if (first === 'endofturn' || first === 'eot') return { kind: 'done' }
  // After `leads`, accept a new side marker or another species for the current side.
  // The `p` side also accepts one `back` clause. The server validates the line.
  if (first === 'leads') {
    return leadsOffersBack(tokens, cursorIndex)
      ? { kind: 'leadsSideSpeciesOrBack' }
      : { kind: 'leadsSideOrSpecies' }
  }

  if (cursorIndex === 1) return { kind: 'action' }

  const action = norm(tokens[1] ?? '')
  if (action === 'switch' || action === 'switchin' || action === 'sendout') {
    return cursorIndex === 2 ? { kind: 'switchSpecies' } : { kind: 'done' }
  }
  if (action === 'mega' || action === 'megaevolve' || action === 'megaevolution') {
    return cursorIndex === 2 ? { kind: 'megaSpeciesOrDone' } : { kind: 'done' }
  }
  if (action === 'tera' || action === 'terastallize' || action === 'terastallized') {
    return cursorIndex === 2 ? { kind: 'teraType' } : { kind: 'done' }
  }
  if (action === 'mustrecharge' || action === 'pass') return { kind: 'done' }
  // The `o1 charging <move>` alias requires the move name.
  // The standard `o1 solarbeam charging` form does not.
  if (action === 'charging') {
    return cursorIndex === 2 ? { kind: 'chargingMove' } : { kind: 'done' }
  }
  if (action === 'hp') return { kind: 'done' } // HP values need no word suggestion.
  if (action === 'illusion' || action === 'illusionended') {
    return cursorIndex === 2 ? { kind: 'switchSpecies' } : { kind: 'done' }
  }
  if (['damage', 'damaged', 'heal', 'healed', 'sethp'].includes(action)) return { kind: 'done' }
  if (action === 'status' || action === 'statusinflicted' || action === 'cure' || action === 'statuscured') {
    return cursorIndex === 2 ? { kind: 'statusWord' } : { kind: 'done' }
  }
  if (action === 'volatileend' || action === 'endvolatile') {
    return cursorIndex === 2 ? { kind: 'volatileWord' } : { kind: 'done' }
  }
  if (action === 'encoremove' || action === 'disablemove') {
    return cursorIndex === 2 ? { kind: 'moveName' } : { kind: 'done' }
  }
  if (action === 'stockpilelevel') {
    return cursorIndex === 2 ? { kind: 'stockpileLevel' } : { kind: 'done' }
  }
  if (action === 'copyboosts' || action === 'boostscopied') {
    return cursorIndex === 2 ? { kind: 'slotSource' } : { kind: 'done' }
  }
  if (action === 'invertboosts' || action === 'boostsinverted') return { kind: 'done' }
  if (ITEM_VERB_WORDS.some((g) => g.aliases.includes(action))) {
    return cursorIndex === 2 ? { kind: 'itemVerbItem' } : { kind: 'done' }
  }
  // A two-token reason, ability, or item line ends here.
  // Longer input treats the second token as a move.
  const previous = norm(tokens[cursorIndex - 1] ?? '')
  const twoBack = norm(tokens[cursorIndex - 2] ?? '')
  const threeBack = norm(tokens[cursorIndex - 3] ?? '')
  if (previous === 'illusion' || previous === 'illusionended') return { kind: 'switchSpecies' }
  if (['damage', 'damaged', 'heal', 'healed', 'sethp'].includes(previous)) return { kind: 'done' }
  if (previous === 'status' || previous === 'statusinflicted' || previous === 'cure' || previous === 'statuscured') {
    return { kind: 'statusWord' }
  }
  if (previous === 'volatileend' || previous === 'endvolatile') return { kind: 'volatileWord' }
  if (previous === 'encoremove' || previous === 'disablemove') return { kind: 'moveName' }
  if (previous === 'stockpilelevel') return { kind: 'stockpileLevel' }
  if (previous === 'copyboosts' || previous === 'boostscopied') return { kind: 'slotSource' }
  if (previous === 'field' || previous === 'pseudoweather') return { kind: 'pseudoWeatherWord' }
  if (twoBack === 'field' || twoBack === 'pseudoweather') return { kind: 'effectState' }
  if (previous === 'side') return { kind: 'sideMarker' }
  if (twoBack === 'side') return { kind: 'sideConditionWord' }
  if (threeBack === 'side') return { kind: 'effectState' }
  return { kind: 'moveBody' }
}

/** Returns ordered suggestions for the token at `cursorIndex`.
 * The first result is the Tab completion. */
export function completionsAt(
  tokens: string[],
  cursorIndex: number,
  partial: string,
  pools: CompletionPools,
): string[] {
  // HP values such as `50%` and `120/200` are complete tokens.
  // Return without unrelated word suggestions.
  if (isSelfCompleteToken(partial)) return []
  const style = styleFromTokens(tokens, pools)
  const recased = recasePools(pools, style)
  const position = classifyPosition(tokens, cursorIndex)
  switch (position.kind) {
    case 'lineStart':
      return rank(
        [
          ...canonicalsOf(SLOT_WORDS),
          ...canonicalsOf(END_OF_TURN_WORDS),
          ...canonicalsOf(FIELD_LINE_WORDS),
          ...canonicalsOf(LEADS_LINE_WORDS),
        ],
        partial,
      )
    case 'action':
      return rank(
        [
          ...canonicalsOf(SLOT_VERB_WORDS),
          ...canonicalsOf(ITEM_VERB_WORDS),
          ...canonicalsOf(CANT_REASON_WORDS),
          ...recased.moves,
          ...recased.abilities,
          ...recased.items,
        ],
        partial,
      )
    case 'weatherWord':
      return rank(canonicalsOf(WEATHER_WORDS), partial)
    case 'terrainWord':
      return rank(canonicalsOf(TERRAIN_WORDS), partial)
    case 'pseudoWeatherWord':
      return rank(canonicalsOf(PSEUDO_WEATHER_WORDS), partial)
    case 'effectState':
      return rank(canonicalsOf(EFFECT_STATE_WORDS), partial)
    case 'sideMarker':
      return rank(['p', 'o'], partial)
    case 'sideConditionWord':
      return rank(canonicalsOf(SIDE_CONDITION_WORDS), partial)
    case 'statusWord':
      return rank(canonicalsOf(STATUS_WORDS), partial)
    case 'volatileWord':
      return rank(canonicalsOf(VOLATILE_WORDS), partial)
    case 'moveName':
      return rank(recased.moves, partial)
    case 'slotSource':
      return rank(SLOT_TARGET_WORDS, partial)
    case 'stockpileLevel':
      return rank(['1', '2', '3'], partial)
    case 'switchSpecies':
    case 'megaSpeciesOrDone':
      return rank(recased.species, partial)
    case 'leadsSideOrSpecies':
      return rank([...canonicalsOf(LEADS_SIDE_WORDS), ...recased.species], partial)
    case 'leadsSideSpeciesOrBack':
      return rank(
        [
          ...canonicalsOf(LEADS_SIDE_WORDS),
          ...canonicalsOf(LEADS_BACK_WORDS),
          ...recased.species,
        ],
        partial,
      )
    case 'teraType':
      return rank(canonicalsOf(TYPE_WORDS), partial)
    case 'itemVerbItem':
      return rank(recased.items, partial)
    case 'chargingMove':
      return rank(recased.moves, partial)
    case 'moveBody':
      return rank(
        [
          ...SLOT_TARGET_WORDS,
          ...EXPLICIT_SLOT_WORDS,
          ...canonicalsOf(MOVE_EFFECT_WORDS),
          ...canonicalsOf(STAT_WORDS),
          ...canonicalsOf(STATUS_WORDS),
          ...canonicalsOf(VOLATILE_WORDS),
          ...canonicalsOf(ITEM_VERB_WORDS),
          ...recased.abilities,
          ...recased.items,
        ],
        partial,
      )
    case 'done':
      return []
  }
}
