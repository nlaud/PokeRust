/** Formats seconds with the clearest microsecond, millisecond, or second unit.
 * The Rust benchmarks use the same format. */
export function formatTime(seconds: number): string {
  if (seconds >= 1) return `${seconds.toFixed(2)} s`
  if (seconds >= 0.001) return `${(seconds * 1000).toFixed(2)} ms`
  return `${Math.round(seconds * 1_000_000)} µs`
}
