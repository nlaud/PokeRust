import { defineConfig } from '@playwright/test'

// Starts the release backend and Vite server for end-to-end tests.
// Vite sends `/api` requests to port 3001.
// `reuseExistingServer` uses servers that a developer already started.
export default defineConfig({
  testDir: './e2e',
  timeout: 30_000,
  expect: { timeout: 5_000 },
  fullyParallel: false,
  // Use one worker for the shared frontend and backend.
  // Parallel input tests can lose simulated keystrokes during CPU contention.
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
      // Run from `poke_rust` because the default dex paths use that directory.
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
