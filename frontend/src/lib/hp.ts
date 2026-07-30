import type { ObservedHp } from '../api/types'

/** Returns the known HP fraction for the HP bar.
 * `maxHp` can be an upper bound in a masked view. */
export function hpFraction(hp: ObservedHp, maxHp: number): number {
  if (hp.exact !== undefined) return maxHp > 0 ? hp.exact / maxHp : 0
  if (hp.percent !== undefined) return hp.percent / 100
  return 0
}

/** Formats exact HP or a masked HP percentage. */
export function hpDisplayText(hp: ObservedHp, maxHp: number): string {
  if (hp.exact !== undefined) return `${hp.exact}/${maxHp}`
  if (hp.percent !== undefined) return `${hp.percent}%`
  return '?'
}
