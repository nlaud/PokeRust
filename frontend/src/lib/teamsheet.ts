/**
 * Extract the species display names from a Showdown-export teamsheet, for
 * sprite previews on team cards. Mirrors the header rules of the backend
 * parser (poke_rust `parse_pokemon_header`): nickname parens, gender markers,
 * and an optional " @ Item" suffix. Display-only — the backend remains the
 * authority on what actually parses.
 */
export function parseSheetSpecies(sheet: string): string[] {
  return sheet
    .replace(/\r\n/g, '\n')
    .split(/\n\s*\n/)
    .map((block) => block.trim())
    .filter(Boolean)
    .map((block) => {
      let header = block.split('\n')[0].trim()
      const atIndex = header.indexOf(' @ ')
      if (atIndex !== -1) header = header.slice(0, atIndex)
      // Nickname form: "Nickname (Species)" — take the parenthesized species.
      // Gender markers "(M)"/"(F)" are not species.
      const parenMatches = [...header.matchAll(/\(([^)]+)\)/g)]
      for (const match of parenMatches.reverse()) {
        const inner = match[1].trim()
        if (inner !== 'M' && inner !== 'F') return inner
      }
      return header.replace(/\((M|F)\)/g, '').trim()
    })
    .filter(Boolean)
}
