import { test, expect } from '@playwright/test'
import type { BenchmarkResponse, TurnSpeedRow, InferenceRow } from '../src/api/types'

// Tests benchmark stream handling with a small mock response.
// The real complete benchmark takes several minutes.

const TURN_SPEED: TurnSpeedRow = {
  scenario: 'singles',
  mode: 'sample',
  rolls: 16,
  crit: true,
  avgTimeSecs: 0.0012,
  // The chart shows this value in the row annotation.
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

/** Builds one complete event stream with a progress event and a result event. */
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
    // Confirm that the chart shows data from the mock row.
    await expect(page.getByText('4 branches')).toBeVisible()
  })
})
