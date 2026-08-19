import { test, expect } from '@playwright/test'
import { pickSelectOption, seedTeam, startTrackerSession } from './helpers'

// Captures the two panels that offer the new PIMC profile.

test.describe('PIMC profile', () => {
  test('the simulator picker offers PIMC under a fog-of-war mode', async ({ page }, testInfo) => {
    await seedTeam(page)
    await page.goto('/simulate')
    await pickSelectOption(page, 'Ruleset', 'Pokémon Champions Season 2 Singles')

    const picker = page.getByTestId('bot-picker')
    // The algorithm list appears only after the profile turns on.
    await picker.getByRole('button', { name: 'None' }).click()
    await page.getByRole('option', { name: 'Solver', exact: true }).click()

    await picker.locator('button[aria-haspopup="listbox"]').nth(1).click()
    await page.getByRole('option', { name: 'PIMC (averaged worlds)' }).click()

    // The particle field belongs to a belief search, so PIMC must show it.
    await expect(picker.locator('label').filter({ hasText: 'Particles' })).toBeVisible()
    await expect(picker.getByTestId('bot-algorithm-hint')).toContainText('strategy fusion')
    await page.screenshot({
      path: testInfo.outputPath('pimc-setup-picker.png'),
      fullPage: true,
      animations: 'disabled',
    })
  })

  test('the tracker panel offers PIMC', async ({ page }, testInfo) => {
    await seedTeam(page)
    await startTrackerSession(page)

    const panel = page.getByTestId('tracker-solver-panel')
    await panel.getByTestId('tracker-solver-toggle').click()
    await panel.locator('button[aria-haspopup="listbox"]').first().click()
    await page.getByRole('option', { name: 'PIMC (averaged worlds)' }).click()

    await expect(panel.locator('label').filter({ hasText: 'Particles' })).toBeVisible()
    await page.screenshot({
      path: testInfo.outputPath('pimc-tracker-panel.png'),
      fullPage: true,
      animations: 'disabled',
    })
  })
})
