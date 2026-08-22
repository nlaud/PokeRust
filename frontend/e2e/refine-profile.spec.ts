import { test, expect } from '@playwright/test'
import { pickSolverSearch, seedTeam } from './helpers'

// Captures the refinement switch.
//
// A refinement pass solves depth 1 over every action, then raises only the cells
// that decide the answer. It needs a search that solves a matrix, so the switch
// appears only when one of the two selected searches can use it.

test.describe('refinement profile', () => {
  test('the advanced limits offer refinement for a matrix search', async ({ page }, testInfo) => {
    await seedTeam(page)
    await page.goto('/simulate')
    await pickSolverSearch(page, 'perfect', 'Double Oracle (exact)')

    await page.getByRole('button', { name: 'Open settings' }).click()
    await page.getByRole('button', { name: 'Advanced limits' }).click()

    const refine = page.getByLabel('Refine to depth')
    await expect(refine).toBeVisible()
    await expect(refine).not.toBeChecked()
    await refine.check()
    await expect(refine).toBeChecked()

    await page.screenshot({
      path: testInfo.outputPath('refine-advanced-limits.png'),
      fullPage: true,
      animations: 'disabled',
    })
  })

  test('a pair of sampled searches hides the refinement switch', async ({ page }) => {
    await seedTeam(page)
    await page.goto('/simulate')
    // Neither selection solves a matrix, so neither holds a support to raise.
    await pickSolverSearch(page, 'imperfect', 'ISMCTS (sampled belief)')
    await pickSolverSearch(page, 'perfect', 'MCTS (sampled)')

    await page.getByRole('button', { name: 'Open settings' }).click()
    await page.getByRole('button', { name: 'Advanced limits' }).click()
    await expect(page.getByLabel('Refine to depth')).toHaveCount(0)
  })
})
