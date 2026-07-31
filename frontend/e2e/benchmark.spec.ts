import { test, expect } from '@playwright/test'
import type {
  BenchmarkProgress,
  BenchmarkResult,
  InferenceRow,
  SolverRow,
  TurnSpeedRow,
} from '../src/api/types'

// Tests benchmark stream handling with a small mock response.
// The real complete benchmark takes several minutes.
// The mock repeats the server contract: one `progress` event, one `result`
// event for each sweep, then one `done` event.

const TURN_SPEED_ROWS: TurnSpeedRow[] = [
  {
    scenario: 'singles',
    mode: 'enumerate',
    rolls: 16,
    crit: true,
    avgTimeSecs: 0.0012,
    // The chart shows this value in the `branches` column.
    avgBranches: 48,
    pairings: 2,
  },
  {
    scenario: 'singles',
    mode: 'sample',
    rolls: 16,
    crit: true,
    avgTimeSecs: 0.0002,
    avgBranches: 1,
    pairings: 2,
  },
  {
    scenario: 'doubles',
    mode: 'enumerate',
    rolls: 1,
    crit: false,
    avgTimeSecs: 0.05,
    avgBranches: 900,
    pairings: 1,
  },
]

const INFERENCE_ROWS: InferenceRow[] = [
  {
    scenario: 'singles',
    informationMode: 'closedSheet',
    calls: 10,
    avgTimeSecs: 0.0005,
    contradictions: 0,
  },
  {
    scenario: 'doubles',
    informationMode: 'openSheet',
    calls: 8,
    avgTimeSecs: 0.0009,
    contradictions: 0,
  },
]

// Give the singles rows matched settings so the pruning card can pair them.
// The pruning bar then shows a 2.00x ratio of turn simulations.
const SOLVER_ROWS: SolverRow[] = [
  {
    scenario: 'singles',
    algorithm: 'backwardInduction',
    depth: 2,
    rolls: 1,
    chance: 'enumerate',
    avgTimeSecs: 0.4,
    avgNodes: 30,
    avgTurnsSimulated: 1200,
    avgCellsEvaluated: 400,
    avgCellsTotal: 400,
    avgLps: 12,
    pairings: 1,
  },
  {
    scenario: 'singles',
    algorithm: 'doubleOracle',
    depth: 2,
    rolls: 1,
    chance: 'enumerate',
    avgTimeSecs: 0.2,
    avgNodes: 25,
    avgTurnsSimulated: 600,
    avgCellsEvaluated: 100,
    avgCellsTotal: 400,
    avgLps: 9,
    pairings: 1,
  },
  {
    scenario: 'doubles',
    algorithm: 'doubleOracle',
    depth: 1,
    rolls: 1,
    chance: 'top4',
    avgTimeSecs: 0.9,
    avgNodes: 12,
    avgTurnsSimulated: 300,
    avgCellsEvaluated: 80,
    avgCellsTotal: 200,
    avgLps: 4,
    pairings: 1,
  },
]

const SWEEPS: { progress: BenchmarkProgress; result: BenchmarkResult }[] = [
  {
    progress: { stage: 'turnSpeed', completed: 3, total: 3 },
    result: { sweep: 'turnSpeed', rows: TURN_SPEED_ROWS },
  },
  {
    progress: { stage: 'inference', completed: 2, total: 2 },
    result: { sweep: 'inference', rows: INFERENCE_ROWS },
  },
  {
    progress: { stage: 'solver', completed: 3, total: 3 },
    result: { sweep: 'solver', rows: SOLVER_ROWS },
  },
]

/** Builds one complete event stream in the order that the server uses.
 * The trailing `done` event closes the stream without a connection error. */
function sseBody(): string {
  const event = (name: string, data: unknown) =>
    `event: ${name}\ndata: ${JSON.stringify(data)}\n\n`
  return (
    SWEEPS.map(({ progress, result }) =>
      event('progress', progress) + event('result', result),
    ).join('') +
    event('done', {})
  )
}

test.describe('Benchmark page (mocked)', () => {
  test('running the sweep renders charts from the mocked SSE result', async ({ page }) => {
    // The application scrolls inside `main`.
    // Use a tall viewport so the screenshot includes all seven cards.
    await page.setViewportSize({ width: 1280, height: 900 })

    await page.route('**/api/benchmark', async (route) => {
      await route.fulfill({
        status: 200,
        contentType: 'text/event-stream',
        body: sseBody(),
      })
    })

    await page.goto('/benchmark')
    await page.getByRole('button', { name: 'Run benchmark' }).click()

    // The page shows one card for each scenario plus the pruning card.
    const cards = page.getByTestId('chart-card')
    await expect(cards).toHaveCount(7, { timeout: 10_000 })
    // Every sweep reported, so no card may stay in `running` or `failed`.
    for (const card of await cards.all()) {
      await expect(card).toHaveAttribute('data-status', 'done')
    }
    // A `done` event must not leave a stream error behind.
    await expect(page.getByText('Connection to server lost')).toHaveCount(0)
    await expect(page.getByRole('button', { name: 'Run again' })).toBeEnabled()

    // Confirm that each card shows the numbers from its own mock rows.
    // Every check uses `exact` because each bar carries a `<title>` that
    // repeats the same numbers in a sentence.
    const turnSpeedSingles = cards.nth(0)
    await expect(turnSpeedSingles.getByText('enumerate · 16 rolls +crit', { exact: true })).toBeVisible()
    await expect(turnSpeedSingles.getByText('1.20 ms', { exact: true })).toBeVisible()
    await expect(turnSpeedSingles.getByText('48', { exact: true })).toBeVisible()
    // The doubles rows belong to the second turn-speed card.
    await expect(turnSpeedSingles.getByText('900', { exact: true })).toHaveCount(0)

    const turnSpeedDoubles = cards.nth(1)
    await expect(turnSpeedDoubles.getByText('enumerate · 1 roll', { exact: true })).toBeVisible()
    await expect(turnSpeedDoubles.getByText('900', { exact: true })).toBeVisible()

    const inferenceSingles = cards.nth(2)
    await expect(inferenceSingles.getByText('closedSheet', { exact: true })).toBeVisible()
    await expect(inferenceSingles.getByText('500 µs', { exact: true })).toBeVisible()

    const solverSingles = cards.nth(4)
    await expect(solverSingles.getByText('d2 · 1 roll · enumerate', { exact: true })).toBeVisible()
    // 100 of 400 evaluated cells is the pruning share for this row.
    await expect(solverSingles.getByText('25%', { exact: true })).toBeVisible()

    // The pruning card divides the backward-induction turns by the double-oracle turns.
    const pruning = cards.nth(6)
    await expect(pruning.getByText('DO · S d2 · enumerate', { exact: true })).toBeVisible()
    await expect(pruning.getByText('2.00×', { exact: true })).toBeVisible()
    await expect(pruning).toBeInViewport({ ratio: 1 })

    await page.screenshot({ path: 'e2e/screenshots/04-benchmark-mocked.png', fullPage: true })
  })
})
