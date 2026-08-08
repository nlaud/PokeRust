import { test, expect } from '@playwright/test'
import { pickSelectOption, seedTeam } from './helpers'

// Tests simulator mode against the real server.
// The test covers team preview, both hotseat players, and turn resolution.

test.describe('Simulate mode', () => {
  test('meta generation uses the format roster size', async ({ page }) => {
    await page.addInitScript(() => {
      localStorage.setItem(
        'pokerust.formats.v2',
        JSON.stringify({
          formats: [
            {
              id: 'four-mon-singles',
              name: 'Four-mon Singles',
              activePokemon: 1,
              totalPokemon: 4,
              broughtPokemon: 3,
              bannedItems: [],
              forceMaxIvs: true,
              teraEnabled: false,
              megaEnabled: true,
              favorite: false,
            },
          ],
        }),
      )
    })
    await page.goto('/simulate')

    // Every list box stays in the document, so the option must come from the
    // open list box. `aria-expanded` marks the one trigger that is open.
    const openOption = (name: string) =>
      page
        .locator('div.relative:has(> button[aria-expanded="true"])')
        .getByRole('option', { name })

    // The first remaining "Saved team" trigger is player 1, then player 2.
    await page.getByRole('button', { name: 'Saved team' }).first().click()
    await openOption('Generate from meta').click()
    await page.getByRole('button', { name: 'Saved team' }).first().click()
    await openOption('Generate from meta').click()
    await page.getByRole('button', { name: 'Start Battle' }).click()

    await expect(page.getByTestId('preview-mon')).toHaveCount(4, { timeout: 10_000 })
  })

  test('team preview and a full turn resolve for both players', async ({ page }) => {
    await seedTeam(page)
    await page.goto('/simulate')

    // Use the one stored team for both players.
    await pickSelectOption(page, 'Ruleset', 'Pokémon Champions Season 2 Singles')
    await page.getByRole('button', { name: 'Start Battle' }).click()

    // P1 selects three Pokémon in order and confirms the team.
    const previewMons = page.getByTestId('preview-mon')
    await expect(previewMons.first()).toBeVisible({ timeout: 10_000 })
    const p1Count = await previewMons.count()
    for (let i = 0; i < p1Count; i++) await previewMons.nth(i).click()
    await page.getByTestId('preview-confirm').click()

    // P2 selects the same team. The battle then starts.
    await expect(page.getByTestId('preview-mon').first()).toBeVisible()
    const p2Count = await page.getByTestId('preview-mon').count()
    for (let i = 0; i < p2Count; i++) await page.getByTestId('preview-mon').nth(i).click()
    await page.getByTestId('preview-confirm').click()

    // Singles has one legal target.
    // Each move commits without a target selection.
    // After P1 selects a move, the panel changes to P2.
    await expect(page.getByTestId('move-option').first()).toBeVisible({ timeout: 10_000 })
    await expect(page.getByRole('button', { name: /Tera/ })).toHaveCount(0)
    await page.getByTestId('move-option').first().click()
    await expect(page.getByTestId('move-option').first()).toBeVisible({ timeout: 10_000 })
    await page.getByTestId('move-option').first().click()

    // Submit the turn after both players select commands.
    // Then confirm the log and the next P1 input.
    await expect(page.getByText('Turn 1', { exact: true })).toBeVisible({ timeout: 15_000 })
    await expect(page.getByTestId('move-option').first()).toBeVisible()
  })

  test('the P2 solver picker resolves an honest profile badge', async ({ page }, testInfo) => {
    await seedTeam(page)
    await page.goto('/simulate')
    await pickSelectOption(page, 'Ruleset', 'Pokémon Champions Season 2 Singles')
    const turnRequests: Record<string, unknown>[] = []
    let analysisRequests = 0
    page.on('request', (request) => {
      if (request.url().endsWith('/analysis')) analysisRequests += 1
      if (request.url().endsWith('/turn')) {
        turnRequests.push(request.postDataJSON() as Record<string, unknown>)
      }
    })

    // Add a supported API override that the setup panel does not expose.
    // This checks both the resolved adjustment and a subsecond time label.
    await page.route('**/api/battles', async (route) => {
      const request = route.request().postDataJSON()
      request.botP2.timeMs = 1
      await route.continue({ postData: JSON.stringify(request) })
    })

    // The profile section uses a span label. Scope each list box by its test ID.
    const picker = page.getByTestId('bot-picker')
    const openOption = (name: string) =>
      picker.locator('div.relative:has(> button[aria-expanded="true"])').getByRole('option', { name })

    // The algorithm list box appears only after the user selects a profile.
    await expect(picker.getByRole('button', { name: 'None' })).toBeVisible()
    await picker.getByRole('button', { name: 'None' }).click()
    await openOption('Fast').click()
    await picker.getByRole('button', { name: 'Double Oracle (exact)' }).click()
    await openOption('ISMCTS (sampled belief)').click()
    await expect(picker).toContainText('P2 uses this solver profile')

    // The picker explains what the chosen algorithm does, on the page and in a
    // tooltip on the list-box trigger.
    const hint = picker.getByTestId('bot-algorithm-hint')
    await expect(hint).toContainText('Sampled')
    await expect(hint).toContainText('respects the fog of war')
    await expect(
      picker.locator('button[aria-haspopup="listbox"]').filter({ hasText: 'ISMCTS' }),
    ).toHaveAttribute('title', /Sampled/)

    // An exact algorithm gets its own explanation.
    await picker.getByRole('button', { name: 'ISMCTS (sampled belief)' }).click()
    await openOption('Double Oracle (exact)').click()
    await expect(hint).toContainText('Exact')
    await expect(hint).toContainText('sees through the fog of war')
    await picker.getByRole('button', { name: 'Double Oracle (exact)' }).click()
    await openOption('ISMCTS (sampled belief)').click()

    await expect(
      picker.locator('button[aria-haspopup="listbox"]').filter({ hasText: 'ISMCTS' }),
    ).toHaveAttribute('aria-expanded', 'false')
    await page.screenshot({
      path: testInfo.outputPath('bot-picker.png'),
      fullPage: true,
      animations: 'disabled',
    })

    await page.getByRole('button', { name: 'Start Battle' }).click()

    // The badge names the resolved algorithm, preset, and limits.
    const badge = page.getByTestId('bot-badge')
    await expect(badge).toBeVisible({ timeout: 10_000 })
    await expect(badge).toContainText('ISMCTS')
    await expect(badge).toContainText('Fast')
    await expect(badge).toContainText('sampled algorithm')
    await expect(badge).toContainText('depth 1')
    await expect(badge).toContainText('1 ms')

    // The new badge must not make a narrow viewport overflow horizontally.
    await page.setViewportSize({ width: 390, height: 844 })
    const overflowing = await badge.evaluate((root) => {
      const bounds = root.getBoundingClientRect()
      return [...root.querySelectorAll<HTMLElement>('*')]
        .filter((element) => {
          const rect = element.getBoundingClientRect()
          return rect.left < bounds.left - 1 || rect.right > bounds.right + 1
        })
        .map((element) => ({
          tag: element.tagName,
          testId: element.dataset.testid,
          className: element.className,
          text: element.textContent?.trim().slice(0, 80),
          right: element.getBoundingClientRect().right,
        }))
    })
    expect(overflowing).toEqual([])

    // The panel lists every approximation of the resolved profile.
    await badge.getByRole('button').first().click()
    const detail = page.getByTestId('bot-badge-detail')
    await expect(detail).toContainText('samples trajectories')
    await expect(detail).toContainText('world(s) from the belief')
    await expect(detail).toContainText('timeMs overrides the fast preset: 1')
    const detailOwnsOverlap = await page.evaluate(() => {
      const detail = document.querySelector<HTMLElement>('[data-testid="bot-badge-detail"]')
      const controls = document.querySelector<HTMLElement>('[data-testid="preview-confirm"]')
      if (!detail || !controls) return false
      const detailRect = detail.getBoundingClientRect()
      const controlRect = controls.getBoundingClientRect()
      const left = Math.max(detailRect.left, controlRect.left)
      const right = Math.min(detailRect.right, controlRect.right)
      const top = Math.max(detailRect.top, controlRect.top)
      const bottom = Math.min(detailRect.bottom, controlRect.bottom)
      if (left >= right || top >= bottom) return true
      const topElement = document.elementFromPoint((left + right) / 2, (top + bottom) / 2)
      return !!topElement && detail.contains(topElement)
    })
    expect(detailOwnsOverlap).toBe(true)

    // The open panel covers the controls below it, so close it before the
    // battle input starts.
    await badge.getByRole('button').first().click()
    await expect(detail).toHaveCount(0)
    await page.setViewportSize({ width: 1280, height: 800 })

    // P1 locks the preview command. The server draws P2's command immediately.
    const previewMons = page.getByTestId('preview-mon')
    const p1Count = await previewMons.count()
    for (let i = 0; i < p1Count; i++) await previewMons.nth(i).click()
    await expect(page.getByTestId('preview-confirm')).toHaveText('Start Battle')
    await page.getByTestId('preview-confirm').click()
    await expect(page.getByTestId('move-option').first()).toBeVisible({ timeout: 10_000 })
    expect(turnRequests).toHaveLength(1)
    expect(turnRequests[0]).not.toHaveProperty('p2')
    // Team preview draws no command, so the reveal panel stays away.
    await expect(page.getByTestId('p2-reveal')).toHaveCount(0)
    await page.screenshot({
      path: testInfo.outputPath('bot-preview-resolved.png'),
      fullPage: true,
      animations: 'disabled',
    })

    // Watch for the terminal response before P1 submits a move.
    // This prevents the test from missing the last polling response.
    const terminalAnalysis = page.waitForResponse(async (response) => {
      if (!response.url().endsWith('/analysis')) return false
      const body = (await response.json()) as { phase?: string }
      return body.phase === 'complete' || body.phase === 'failed'
    })

    // P1 locks one move. The client waits for analysis and never asks for P2 input.
    await page.getByTestId('move-option').first().click()
    // The turn can end in a replacement, so assert the log, not a move list.
    const [analysisResponse] = await Promise.all([
      terminalAnalysis,
      expect(page.getByText('Turn 1', { exact: true })).toBeVisible({ timeout: 15_000 }),
    ])
    expect(analysisRequests).toBeGreaterThan(0)
    const analysis = (await analysisResponse.json()) as {
      generation: number
      phase: string
      checkpoint: { generation: number; stale: boolean } | null
      error: string | null
    }
    expect(analysis).toMatchObject({
      phase: 'complete',
      checkpoint: { generation: analysis.generation, stale: false },
      error: null,
    })
    expect(turnRequests).toHaveLength(2)
    expect(turnRequests[1]).not.toHaveProperty('p2')

    // The reveal names P2's one action. It must never show the odds of that
    // action or P2's win probability.
    const reveal = page.getByTestId('p2-reveal')
    await expect(reveal).toBeVisible()
    await expect(reveal).toContainText('Player 2 played')
    await expect(reveal).toContainText('from the solver strategy')
    await reveal.getByRole('button').first().click()
    const revealDetail = page.getByTestId('p2-reveal-detail')
    await expect(revealDetail).toContainText('Draw seed')
    await expect(revealDetail).toContainText('ismcts')
    await expect(revealDetail).not.toContainText('%')
    await expect(revealDetail).not.toContainText(/win/i)
    await page.screenshot({
      path: testInfo.outputPath('bot-turn-resolved.png'),
      fullPage: true,
      animations: 'disabled',
    })
  })

  test('the reveal names the solver strategy under perfect information', async ({
    page,
  }, testInfo) => {
    await seedTeam(page)
    await page.goto('/simulate')
    await pickSelectOption(page, 'Ruleset', 'Pokémon Champions Season 2 Singles')
    // An exact algorithm reads the true position, so it can only control P2
    // when the session hides nothing. A fogged session falls back to the
    // uniform draw.
    await pickSelectOption(page, 'Information mode', 'Perfect Information')

    const picker = page.getByTestId('bot-picker')
    const openOption = (name: string) =>
      picker
        .locator('div.relative:has(> button[aria-expanded="true"])')
        .getByRole('option', { name })
    await picker.getByRole('button', { name: 'None' }).click()
    await openOption('Fast').click()
    await expect(picker.getByTestId('bot-algorithm-hint')).toContainText('Exact')

    await page.getByRole('button', { name: 'Start Battle' }).click()
    const previewMons = page.getByTestId('preview-mon')
    await expect(previewMons.first()).toBeVisible({ timeout: 10_000 })
    const count = await previewMons.count()
    for (let i = 0; i < count; i++) await previewMons.nth(i).click()
    await page.getByTestId('preview-confirm').click()

    // The wait line replaces the reveal while the search runs.
    await expect(page.getByTestId('move-option').first()).toBeVisible({ timeout: 10_000 })
    await page.getByTestId('move-option').first().click()
    await expect(page.getByText('Turn 1', { exact: true })).toBeVisible({ timeout: 20_000 })

    const reveal = page.getByTestId('p2-reveal')
    await expect(reveal).toContainText('from the solver strategy')
    await reveal.getByRole('button').first().click()
    const detail = page.getByTestId('p2-reveal-detail')
    await expect(detail).toContainText('doubleOracle')
    await expect(detail).toContainText('turn 1')
    // The reveal carries one action. It must show no odds and no win rate.
    await expect(detail).not.toContainText('%')
    await expect(detail).not.toContainText(/win/i)
    await page.screenshot({
      path: testInfo.outputPath('bot-reveal-strategy.png'),
      fullPage: true,
      animations: 'disabled',
    })
  })
})
