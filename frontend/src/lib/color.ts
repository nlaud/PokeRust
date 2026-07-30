// Calculates a readable text color for a custom background.
// The contrast calculation follows WCAG.

/** Parses a hexadecimal color into RGB channel values.
 * Returns black when the input is invalid. */
export function hexToRgb(hex: string): [number, number, number] {
  let h = hex.replace('#', '')
  if (h.length === 3) {
    h = h
      .split('')
      .map((c) => c + c)
      .join('')
  }
  const n = parseInt(h, 16)
  if (h.length !== 6 || Number.isNaN(n)) return [0, 0, 0]
  return [(n >> 16) & 255, (n >> 8) & 255, n & 255]
}

/** WCAG relative luminance (0 = black, 1 = white) of an sRGB channel triple. */
function relativeLuminance([r, g, b]: [number, number, number]): number {
  const linear = (c: number) => {
    const s = c / 255
    return s <= 0.03928 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4
  }
  return 0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b)
}

/** WCAG contrast ratio between two relative luminances (order-independent). */
function contrastRatio(l1: number, l2: number): number {
  const lighter = Math.max(l1, l2)
  const darker = Math.min(l1, l2)
  return (lighter + 0.05) / (darker + 0.05)
}

/** Text colors from the light and dark themes. */
const DARK_TEXT = '#0f172a'
const LIGHT_TEXT = '#f1f5f9'

/** Selects the text color with the higher WCAG contrast against `bgHex`. */
export function computeReadableTextColor(bgHex: string): string {
  const bgLuminance = relativeLuminance(hexToRgb(bgHex))
  const darkContrast = contrastRatio(bgLuminance, relativeLuminance(hexToRgb(DARK_TEXT)))
  const lightContrast = contrastRatio(bgLuminance, relativeLuminance(hexToRgb(LIGHT_TEXT)))
  return darkContrast >= lightContrast ? DARK_TEXT : LIGHT_TEXT
}
