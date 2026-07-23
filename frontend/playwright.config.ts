import { defineConfig } from '@playwright/test'

// E2E config for the tracker input bar (see e2e/tracker-input.spec.ts). Spins
// up BOTH halves of the stack the frontend README describes: the release
// server (release matters — turn resolution/inference is compute-heavy) from
// `poke_rust/`, and the Vite dev server here, proxying /api -> :3001 exactly
// like local development does. `reuseExistingServer` lets a developer who
// already has both running locally skip the relaunch.
export default defineConfig({
  testDir: './e2e',
  timeout: 30_000,
  expect: { timeout: 5_000 },
  fullyParallel: false,
  // Every spec shares one dev server + one backend process (see `webServer`
  // below) — `fullyParallel: false` alone only serializes tests WITHIN a
  // file; separate spec files still fan out across workers by default, and
  // real CPU contention between them was observed to occasionally drop a
  // keystroke mid-`pressSequentially` (a space lost between two typed words,
  // changing which grammar position a suggestion was ranked for). One worker
  // keeps the whole suite deterministic.
  workers: 1,
  retries: 0,
  reporter: [['list']],
  use: {
    baseURL: 'http://localhost:5173',
    trace: 'retain-on-failure',
    screenshot: 'only-on-failure',
  },
  webServer: [
    {
      // `cwd` matters (dex file default paths are relative to `poke_rust/`),
      // so the executable itself is addressed relative to that cwd, not to
      // this config file — a `../`-prefixed command here would double up.
      command: '.\\target\\release\\server.exe',
      cwd: '../poke_rust',
      port: 3001,
      timeout: 30_000,
      reuseExistingServer: !process.env.CI,
    },
    {
      command: 'npm run dev',
      port: 5173,
      timeout: 30_000,
      reuseExistingServer: !process.env.CI,
    },
  ],
})
