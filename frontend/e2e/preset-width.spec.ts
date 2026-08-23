import { test, expect } from '@playwright/test'
import { seedTeam } from './helpers'

// Captures the preset row and the damage-roll control.
//
// Every preset runs one turn of lookahead, so the presets differ in width
// rather than in depth. Damage rolls are the main precision control of a
// one-turn search, so the sidebar shows that field beside the presets instead
// of inside the advanced limits.

test('the sidebar shows damage rolls beside the presets', async ({ page }) => {
  await seedTeam(page)
  await page.goto('/simulate')
  await page.getByRole('button', { name: 'Open settings' }).click()

  // The field must be reachable without opening the advanced limits.
  const rolls = page.locator('label').filter({ hasText: 'Damage rolls' }).locator('input')
  await expect(rolls).toBeVisible()

  // Each preset sets its own width. The presets differ in rolls and worlds.
  await page.getByRole('button', { name: 'Fast solver preset' }).click()
  await expect(rolls).toHaveValue('3')
  await page.getByRole('button', { name: 'High solver preset' }).click()
  await expect(rolls).toHaveValue('16')
  await page.getByRole('button', { name: 'Balanced solver preset' }).click()
  await expect(rolls).toHaveValue('8')

  // The advanced limits still hold the depth, and it reads one turn.
  await page.getByRole('button', { name: 'Advanced limits' }).click()
  const depth = page.locator('label').filter({ hasText: 'Depth' }).first().locator('input')
  await expect(depth).toHaveValue('1')

  // Editing a limit makes the profile custom.
  await rolls.fill('12')
  await rolls.blur()
  await expect(page.getByText('Custom solver limits are active.')).toBeVisible()

  // The advanced limits must fit the sidebar. A field that overflows pushes a
  // horizontal scrollbar onto the whole page.
  await expect(rolls).toBeVisible()
  const overflow = await page.evaluate(() => {
    const panel = document.querySelector<HTMLElement>('aside')
    if (!panel) return 0
    return panel.scrollWidth - panel.clientWidth
  })
  expect(overflow).toBeLessThanOrEqual(1)

  await page.screenshot({
    path: 'e2e/screenshots/07-preset-width.png',
    fullPage: true,
    animations: 'disabled',
  })
})
