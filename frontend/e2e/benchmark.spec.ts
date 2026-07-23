import { test, expect } from '@playwright/test'
import type { BenchmarkResponse, TurnSpeedRow, InferenceRow } from '../src/api/types'

// The real sweep (`GET /api/benchmark`) is a genuinely unbounded, multi-minute
// job (see `BenchmarkingPage`'s doc comment / `poke_rust/benches/RESULTS.md`)
// — not something an e2e suite should ever run for real. `page.route` mocks
// the SSE endpoint with a small, fast, schema-accurate response instead, so
// this test covers the CLIENT'S handling of that stream (progress → chart
// rendering) without paying the sweep's real cost.

const TURN_SPEED: TurnSpeedRow = {
  scenario: 'singles',
  mode: 'sample',
  rolls: 16,
  crit: true,
  avgTimeSecs: 0.0012,
  // `turnSpeedChartRows` renders this (not `pairings`) as the row's
  // annotation text — see the assertion below.
  avgBranches: 4,
  pairings: 1,
}

const INFERENCE: InferenceRow = {
  scenario: 'singles',
  informationMode: 'closedSheet',
  calls: 10,
  avgTimeSecs: 0.0005,
  contradictions: 0,
}

const MOCK_RESULT: BenchmarkResponse = {
  turnSpeed: [TURN_SPEED],
  inference: [INFERENCE],
}

/** Builds a complete SSE body: a `progress` event followed by the terminal
 * `result` event, formatted exactly like `EventSource` expects
 * (`event: NAME\ndata: JSON\n\n`, matching `client.ts::streamBenchmark`'s
 * named listeners). `route.fulfill` delivers this as a single already-
 * complete response body (Playwright has no primitive for a slow-drip,
 * artificially delayed stream), so the intermediate "busy + progress bar" UI
 * state is real but too fast to reliably assert on here — this test verifies
 * the terminal, settled state the mocked stream produces instead. */
function sseBody(): string {
  const progress = { stage: 'turnSpeed', completed: 1, total: 2 }
  return (
    `event: progress\ndata: ${JSON.stringify(progress)}\n\n` +
    `event: result\ndata: ${JSON.stringify(MOCK_RESULT)}\n\n`
  )
}

test.describe('Benchmark page (mocked)', () => {
  test('running the sweep renders charts from the mocked SSE result', async ({ page }) => {
    await page.route('**/api/benchmark', async (route) => {
      await route.fulfill({
        status: 200,
        contentType: 'text/event-stream',
        body: sseBody(),
      })
    })

    await page.goto('/benchmark')
    await page.getByRole('button', { name: 'Run benchmark' }).click()

    await expect(page.getByText('Turn Speed: Singles', { exact: true })).toBeVisible({
      timeout: 10_000,
    })
    await expect(page.getByText('Inference: Singles', { exact: true })).toBeVisible()
    // Confirms the mocked row's own data actually reached the chart, not
    // just that SOME chart rendered.
    await expect(page.getByText('4 branches')).toBeVisible()
  })
})
