import type { ObservedHp } from '../api/types'

/** Fraction of max HP in [0, 1], for HP-bar width. `maxHp` is the display side's
 * best-known upper bound on max HP (`statsMax[0]`) — exact for a known side, an
 * upper bound under a masked opponent view. */
export function hpFraction(hp: ObservedHp, maxHp: number): number {
  if (hp.exact !== undefined) return maxHp > 0 ? hp.exact / maxHp : 0
  if (hp.percent !== undefined) return hp.percent / 100
  return 0
}

/** Display text for an HP value: exact "current/max" for a known side, "NN%" for a
 * masked opponent whose exact HP a real player never sees. */
export function hpDisplayText(hp: ObservedHp, maxHp: number): string {
  if (hp.exact !== undefined) return `${hp.exact}/${maxHp}`
  if (hp.percent !== undefined) return `${hp.percent}%`
  return '?'
}
