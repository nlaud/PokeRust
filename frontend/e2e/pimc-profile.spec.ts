import { test, expect } from '@playwright/test'
import { pickSelectOption, pickSolverSearch, seedTeam, startTrackerSession } from './helpers'

// Captures the two panels that offer the new PIMC profile.

test.describe('PIMC profile', () => {
  test('the simulator picker offers PIMC under a fog-of-war mode', async ({ page }, testInfo) => {
    await seedTeam(page)
    await page.goto('/simulate')
    await pickSelectOption(page, 'Ruleset', 'Pokémon Champions Season 2 Singles')
    // A fog-of-war mode hides data, so the battle reads the
    // imperfect-information search from the settings sidebar.
    await pickSolverSearch(page, 'imperfect', 'PIMC (averaged worlds)')

    const picker = page.getByTestId('bot-picker')
    // The resolved search appears only after the profile turns on.
    await picker.getByRole('button', { name: 'None' }).click()
    await picker.getByRole('option', { name: 'Solver', exact: true }).click()

    await expect(picker.getByTestId('bot-algorithm-name')).toHaveText('PIMC (averaged worlds)')
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

    // The search picker and the advanced limits both live in the settings
    // sidebar, so the particle field appears there rather than in the panel.
    await pickSolverSearch(page, 'imperfect', 'PIMC (averaged worlds)')

    const panel = page.getByTestId('tracker-solver-panel')
    await panel.getByTestId('tracker-solver-toggle').click()
    await expect(panel.getByTestId('tracker-solver-algorithm')).toContainText(
      'PIMC (averaged worlds)',
    )
    await expect(panel).toContainText('claims more than a real player can do')
    await page.screenshot({
      path: testInfo.outputPath('pimc-tracker-panel.png'),
      fullPage: true,
      animations: 'disabled',
    })
  })
})
