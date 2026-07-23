// Word-by-word completion engine for the tracker input bar (Minecraft-chat-
// style command completion). This is a COMPLETION-ONLY mirror of the
// authoritative grammar in `poke_rust/src/bin/server/tracker_parse.rs` — it
// never builds an `InformationEvent`, only ranks candidate next-tokens, and
// it is NOT the source of truth: the server's parser is, and a submission
// that slips past this mirror still gets a real 422 `{line, message}` from
// `POST /api/tracker/{id}/events` (or `/preview`, `/history`). Keep the
// tables below in sync with `tracker_parse.rs`'s `status_from_word` /
// `cant_reason_from_word` / `volatile_from_word` / `weather_from_word` /
// `terrain_from_word` / `item_verb_from_word` / `stat_idx` / `parse_line`'s
// dispatch order — the same "two hand-synced sources" discipline the DTOs in
// `api/types.ts` already follow (see `frontend/README.md`).

/** Mirrors `tracker_parse.rs::norm` exactly: alphanumeric-only, lowercased —
 * the same normalization `Species::from_str`/`PokemonMove::from_str`/
 * `Item::from_str`/`Ability::from_str` all do internally on the Rust side. */
export function norm(s: string): string {
  return s
    .toLowerCase()
    .split('')
    .filter((c) => /[a-z0-9]/.test(c))
    .join('')
}

/** Per-match name pools an autocomplete session needs: species/moves/
 * abilities come from `GET /api/tracker/{id}/completions` (scoped to the
 * Pokemon actually in this match — see that endpoint's doc comment in
 * `dto.rs`); items are NOT match-scoped (held items aren't
 * species-constrained) so they come straight from the existing
 * `lib/items.ts` catalog instead of a new backend round trip. */
export interface CompletionPools {
  species: string[]
  moves: string[]
  abilities: string[]
  items: string[]
}

export const EMPTY_POOLS: CompletionPools = { species: [], moves: [], abilities: [], items: [] }

// ── Fixed keyword tables, mirroring tracker_parse.rs ────────────────────────
// Each concept lists every word the Rust parser accepts; `canonical` is the
// single word this module SUGGESTS for that concept (kept short/memorable so
// the rising suggestion list doesn't show five spellings of the same thing —
// every alias below still round-trips through the server either way).

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

// Bare-word (no payload) volatiles only — mirrors `volatile_from_word` exactly,
// including its documented gap (payload-bearing volatiles like `Disable(move)`
// aren't reachable this way; see that function's doc comment).
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
]

const SLOT_VERB_WORDS: WordGroup[] = [
  group('switch', 'switchin', 'sendout'),
  group('leads'),
  group('mega', 'megaevolve', 'megaevolution'),
  group('tera', 'terastallize', 'terastallized'),
  group('mustrecharge'),
  group('pass'),
  group('hp'),
]

// Champions' 18 damage types plus the Tera-only Stellar type (`tera <type>`
// accepts it like any other type word — mirrors `dex_data::parse_type`).
const TYPE_WORDS: WordGroup[] = [
  'Normal', 'Fire', 'Water', 'Electric', 'Grass', 'Ice', 'Fighting', 'Poison',
  'Ground', 'Flying', 'Psychic', 'Bug', 'Rock', 'Ghost', 'Dragon', 'Dark',
  'Steel', 'Fairy', 'Stellar',
].map((t) => group(t))

const SLOT_WORDS: WordGroup[] = [group('p'), group('p1'), group('p2'), group('o'), group('o1'), group('o2')]
const END_OF_TURN_WORDS: WordGroup[] = [group('endofturn', 'eot')]
const FIELD_LINE_WORDS: WordGroup[] = [group('weather'), group('terrain')]

function canonicalsOf(groups: WordGroup[]): string[] {
  return groups.map((g) => g.canonical)
}

// ── Levenshtein autocorrect fallback ────────────────────────────────────────

function levenshtein(a: string, b: string): number {
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

/** Rank `candidates` against `partial`: exact prefix matches first (in list
 * order), then — only if NO candidate has `partial` as a prefix — every
 * candidate ranked by edit distance to `partial`, closest first. This is the
 * "autocorrect spelling to the closest possibility" behavior: a typo like
 * `thunderblot` still surfaces `Thunderbolt` as the top (Tab-fillable)
 * suggestion instead of an empty list. */
function rank(candidates: string[], partial: string): string[] {
  const p = norm(partial)
  if (p === '') return candidates
  const prefixMatches = candidates.filter((c) => norm(c).startsWith(p))
  if (prefixMatches.length > 0) return prefixMatches
  return [...candidates].sort((a, b) => levenshtein(norm(a), p) - levenshtein(norm(b), p))
}

// ── Script (de)serialization ─────────────────────────────────────────────────

/** Split a full raw tracker-text script (as returned by `GetTrackerResponse.script`
 * / sent to `PUT /history`) into one chunk of text per turn, each ending at
 * (and including) its `endofturn`/`eot` line. Lightweight and independent of
 * the real parser — used only to rehydrate the editor's per-turn navigation
 * after a page reload, never to decide correctness (the server is still the
 * sole authority on what's valid). Trailing text after the last `endofturn`
 * (a script that was never fully committed) is returned as a final,
 * not-yet-terminated chunk rather than dropped, so a reload never silently
 * loses typed-but-uncommitted text. */
/** The content lines of one turn's raw text, with a trailing `endofturn`/`eot`
 * line (and any blank lines around it) stripped off — the addressable,
 * user-authored lines within that turn. Shared by the store (to rebuild a
 * corrected turn) and the input bar (to build its flat navigation history). */
export function contentLinesOf(turnText: string): string[] {
  const lines = turnText.split('\n')
  while (lines.length > 0 && lines[lines.length - 1].trim() === '') lines.pop()
  if (lines.length > 0) {
    const last = norm(lines[lines.length - 1])
    if (last === 'endofturn' || last === 'eot') lines.pop()
  }
  return lines
}

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
  | { kind: 'switchSpecies' }
  | { kind: 'leadsSpecies' }
  | { kind: 'megaSpeciesOrDone' }
  | { kind: 'teraType' }
  | { kind: 'itemVerbItem' }
  | { kind: 'moveBody' }
  | { kind: 'done' }

/** Classify what the token AT `tokens[cursorIndex]` (the word being typed)
 * is for, from the already-completed tokens before it. Mirrors `parse_line`'s
 * dispatch order (`tracker_parse.rs:568-864`) and `parse_move_line`'s body
 * loop (`:962-1052`) closely enough to drive completion, without attempting
 * every validation branch the real parser runs (unresolvable species, mega
 * suffix disambiguation, etc. — those still surface as a real 422 on submit). */
export function classifyPosition(tokens: string[], cursorIndex: number): LinePosition {
  if (cursorIndex === 0) return { kind: 'lineStart' }

  const first = norm(tokens[0] ?? '')
  if (first === 'weather') return cursorIndex === 1 ? { kind: 'weatherWord' } : { kind: 'done' }
  if (first === 'terrain') return cursorIndex === 1 ? { kind: 'terrainWord' } : { kind: 'done' }
  if (first === 'endofturn' || first === 'eot') return { kind: 'done' }

  if (cursorIndex === 1) return { kind: 'action' }

  const action = norm(tokens[1] ?? '')
  if (action === 'switch' || action === 'switchin' || action === 'sendout') {
    return cursorIndex === 2 ? { kind: 'switchSpecies' } : { kind: 'done' }
  }
  if (action === 'leads') return { kind: 'leadsSpecies' }
  if (action === 'mega' || action === 'megaevolve' || action === 'megaevolution') {
    return cursorIndex === 2 ? { kind: 'megaSpeciesOrDone' } : { kind: 'done' }
  }
  if (action === 'tera' || action === 'terastallize' || action === 'terastallized') {
    return cursorIndex === 2 ? { kind: 'teraType' } : { kind: 'done' }
  }
  if (action === 'mustrecharge' || action === 'pass') return { kind: 'done' }
  if (action === 'hp') return { kind: 'done' } // numeric hpspec — nothing to suggest from a word list
  if (ITEM_VERB_WORDS.some((g) => g.aliases.includes(action))) {
    return cursorIndex === 2 ? { kind: 'itemVerbItem' } : { kind: 'done' }
  }
  // A bare 2-token cant-reason/ability/item line ends here; anything longer
  // means `tokens[1]` was a MOVE (the same length-sensitive collision
  // `tracker_parse.rs:810-827` resolves) — the move-line body loop applies
  // for every token from index 2 onward, including inline item-verb targets.
  return { kind: 'moveBody' }
}

/** Ordered suggestions for the word currently being typed at `cursorIndex`
 * (0-based token index; the token being edited, not yet committed), given the
 * already-typed `tokens` before it and the current partial text. Top of the
 * returned array is the Tab-fill target. */
export function completionsAt(
  tokens: string[],
  cursorIndex: number,
  partial: string,
  pools: CompletionPools,
): string[] {
  const position = classifyPosition(tokens, cursorIndex)
  switch (position.kind) {
    case 'lineStart':
      return rank(
        [...canonicalsOf(SLOT_WORDS), ...canonicalsOf(END_OF_TURN_WORDS), ...canonicalsOf(FIELD_LINE_WORDS)],
        partial,
      )
    case 'action':
      return rank(
        [
          ...canonicalsOf(SLOT_VERB_WORDS),
          ...canonicalsOf(ITEM_VERB_WORDS),
          ...canonicalsOf(CANT_REASON_WORDS),
          ...pools.moves,
          ...pools.abilities,
          ...pools.items,
        ],
        partial,
      )
    case 'weatherWord':
      return rank(canonicalsOf(WEATHER_WORDS), partial)
    case 'terrainWord':
      return rank(canonicalsOf(TERRAIN_WORDS), partial)
    case 'switchSpecies':
    case 'leadsSpecies':
    case 'megaSpeciesOrDone':
      return rank(pools.species, partial)
    case 'teraType':
      return rank(canonicalsOf(TYPE_WORDS), partial)
    case 'itemVerbItem':
      return rank(pools.items, partial)
    case 'moveBody':
      return rank(
        [
          ...SLOT_WORDS.filter((g) => g.canonical !== 'p' && g.canonical !== 'o').map((g) => g.canonical),
          ...canonicalsOf(MOVE_EFFECT_WORDS),
          ...canonicalsOf(STAT_WORDS),
          ...canonicalsOf(STATUS_WORDS),
          ...canonicalsOf(VOLATILE_WORDS),
          ...canonicalsOf(ITEM_VERB_WORDS),
          ...pools.abilities,
          ...pools.items,
        ],
        partial,
      )
    case 'done':
      return []
  }
}
