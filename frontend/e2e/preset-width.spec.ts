import { test, expect } from '@playwright/test'
import { seedTeam } from './helpers'

// Captures the preset row and the damage-roll control.
//
// A preset holds two values for each limit, because the two search families
// have different cost models. The sidebar shows the value of the fog-of-war
// search, which is the one that every information mode except Perfect
// Information runs.
//
// A belief search draws one outcome for each turn, so a damage roll costs it
// almost nothing and every preset reads all sixteen. A search that enumerates a
// turn pays for each roll, so it reads far fewer.

test('the sidebar shows damage rolls beside the presets', async ({ page }) => {
  await seedTeam(page)
  await page.goto('/simulate')
  await page.getByRole('button', { name: 'Open settings' }).click()

  // The field must be reachable without opening the advanced limits. It names
  // the family it edits, because the two families take different counts.
  const rolls = page.locator('label').filter({ hasText: 'Damage rolls' }).locator('input')
  await expect(rolls).toBeVisible()
  await expect(page.getByText('Damage rolls, sampled (1-16)')).toBeVisible()

  // A roll is close to free for a belief search, so no preset gives up a roll.
  // The presets differ in the seconds of one answer and in the worlds they draw.
  for (const preset of ['Fast', 'High', 'Balanced']) {
    await page.getByRole('button', { name: `${preset} solver preset` }).click()
    await expect(rolls).toHaveValue('16')
  }

  // The advanced limits hold one depth for each family.
  await page.getByRole('button', { name: 'Advanced limits' }).click()
  const exactDepth = page.locator('label').filter({ hasText: 'Depth, exact' }).locator('input')
  const sampledDepth = page.locator('label').filter({ hasText: 'Depth, sampled' }).locator('input')
  // An exact search multiplies its tree for each ply, so it stays at one turn.
  await expect(exactDepth).toHaveValue('1')
  // A belief search adds one draw for each ply, so it can afford lookahead.
  await expect(sampledDepth).toHaveValue('2')

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
