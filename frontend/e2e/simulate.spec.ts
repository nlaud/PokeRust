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
    await expect(picker).toContainText('P2 stays under hotseat control for now.')
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

    // A profile does not run the planned bot yet. P2 still gets hotseat input.
    const previewMons = page.getByTestId('preview-mon')
    const p1Count = await previewMons.count()
    for (let i = 0; i < p1Count; i++) await previewMons.nth(i).click()
    await page.getByTestId('preview-confirm').click()
    await expect(page.getByTestId('preview-confirm')).toHaveText('Start Battle')
    await page.screenshot({ path: testInfo.outputPath('bot-badge.png') })
  })
})
