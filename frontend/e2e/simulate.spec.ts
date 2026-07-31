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
})
