/** Extracts species names from a Showdown teamsheet for sprite previews.
 * This function follows the backend header rules.
 * The backend remains the authority for valid teamsheets. */
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
      // Use the parenthesized species in `Nickname (Species)`.
      // Do not treat `(M)` or `(F)` as a species.
      const parenMatches = [...header.matchAll(/\(([^)]+)\)/g)]
      for (const match of parenMatches.reverse()) {
        const inner = match[1].trim()
        if (inner !== 'M' && inner !== 'F') return inner
      }
      return header.replace(/\((M|F)\)/g, '').trim()
    })
    .filter(Boolean)
}
