/** Formats seconds as µs/ms/s, whichever reads best — mirrors
 * `poke_rust/benches/bench_common.rs::fmt_time` so the frontend and the
 * offline `cargo bench` output use the same convention. */
export function formatTime(seconds: number): string {
  if (seconds >= 1) return `${seconds.toFixed(2)} s`
  if (seconds >= 0.001) return `${(seconds * 1000).toFixed(2)} ms`
  return `${Math.round(seconds * 1_000_000)} µs`
}
