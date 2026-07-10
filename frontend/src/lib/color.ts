// Small color-math helpers for the custom theme: given a user-picked background
// color, compute a text color that stays readable against it (WCAG contrast),
// rather than letting the user pick a color that could make text illegible.

/** Parses a `#rgb` or `#rrggbb` hex string into 0–255 channel values. Falls back to
 * black on malformed input rather than throwing — this feeds a `<input type="color">`
 * value, which browsers always emit as valid 6-digit hex, but stay defensive since it
 * also round-trips through localStorage. */
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

/** Near-black (light theme's `--text-primary`) and near-white (dark theme's) — reuse
 * the app's own existing text colors as the two candidates rather than pure #000/#fff,
 * so custom-themed text matches the visual weight of the built-in themes. */
const DARK_TEXT = '#0f172a'
const LIGHT_TEXT = '#f1f5f9'

/** Picks whichever of `DARK_TEXT`/`LIGHT_TEXT` has the higher WCAG contrast ratio
 * against `bgHex` — more robust than a flat luminance threshold, since it directly
 * optimizes for the property that actually matters (readability). */
export function computeReadableTextColor(bgHex: string): string {
  const bgLuminance = relativeLuminance(hexToRgb(bgHex))
  const darkContrast = contrastRatio(bgLuminance, relativeLuminance(hexToRgb(DARK_TEXT)))
  const lightContrast = contrastRatio(bgLuminance, relativeLuminance(hexToRgb(LIGHT_TEXT)))
  return darkContrast >= lightContrast ? DARK_TEXT : LIGHT_TEXT
}
