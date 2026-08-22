import { test, expect } from '@playwright/test'
import { pickSelectOption, pickSolverSearch, seedTeam } from './helpers'

// Captures what a refined answer shows on the simulate page.
//
// A refined answer reaches a depth that a complete search cannot, and it reaches
// that depth over part of the action set. A depth alone would read as a complete
// search to that depth, so the card must also say that the depth came from
// refinement, and it must carry the notes of the answer.
//
// The position here is Singles, because the seeded team brings three Pokemon.
// The display is the same for either format.

test('a refined answer names its refinement and carries its notes', async ({ page }, testInfo) => {
  test.setTimeout(120_000)

  await seedTeam(page)
  await page.goto('/simulate')
  await pickSelectOption(page, 'Ruleset', 'Pokémon Champions Season 2 Singles')
  await pickSolverSearch(page, 'perfect', 'Double Oracle (exact)')
  // Perfect Information reads the perfect-information search above.
  await pickSelectOption(page, 'Information mode', 'Perfect Information')

  await page.getByRole('button', { name: 'Open settings' }).click()
  await page.getByRole('button', { name: 'Advanced limits' }).click()
  await page.getByLabel('Refine to depth').check()
  // A budget this small cannot verify every action, so the answer must carry a
  // note that says so.
  const turns = page.locator('label').filter({ hasText: 'Simulation turns' }).locator('input')
  await turns.fill('200')
  await turns.blur()
  await page.getByRole('button', { name: 'Close settings' }).click()

  const picker = page.getByTestId('bot-picker')
  await picker.getByRole('button', { name: 'None' }).click()
  await picker.getByRole('option', { name: 'Solver', exact: true }).click()

  await page.getByRole('button', { name: 'Saved team' }).first().click()
  await page.getByRole('button', { name: 'Saved team' }).first().click()
  await page.getByRole('button', { name: 'Start Battle' }).click()

  const badge = page.getByTestId('bot-badge')
  await expect(badge).toBeVisible({ timeout: 60_000 })
  await badge.getByRole('button').first().click()

  const detail = page.getByTestId('bot-badge-detail')
  // The profile line must name the refinement next to the depth.
  await expect(detail).toContainText('refined from depth 1')

  // The answer line must name the depth it reached, and the notes of the answer
  // must be reachable from it.
  const odds = page.getByTestId('bot-win-odds')
  await expect(odds).toBeVisible({ timeout: 60_000 })
  await expect(odds).toContainText('depth')

  const notes = page.getByTestId('bot-answer-notes')
  await expect(notes).toBeVisible()
  await notes.getByRole('button').hover()
  await expect(notes).toContainText(/budget|verified|round/)

  await page.screenshot({
    path: testInfo.outputPath('refine-answer.png'),
    fullPage: true,
    animations: 'disabled',
  })
})
