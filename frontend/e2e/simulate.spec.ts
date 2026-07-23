import { test, expect } from '@playwright/test'
import { pickSelectOption, seedTeam } from './helpers'

// End-to-end coverage for battle ("Simulate") mode: the hotseat command
// wizard (team preview → per-slot move selection for BOTH players → turn
// resolution), against the real server — no mocking. Complements
// tracker-input.spec.ts, which covers the OTHER way to drive the fog-of-war
// engine (typed events instead of a simulated opponent).

test.describe('Simulate mode', () => {
  test('team preview and a full turn resolve for both players', async ({ page }) => {
    await seedTeam(page)
    await page.goto('/simulate')

    // Only one team is seeded — SetupPanel's defaults fall back to using it
    // for both P1 and P2, which is a perfectly legal (if unusual) battle.
    await pickSelectOption(page, 'Ruleset', 'Pokémon Champions Season 2 Singles')
    await page.getByRole('button', { name: 'Start Battle' }).click()

    // ── Team preview: P1 picks all 3 brought mons in order, confirms. ──────
    const previewMons = page.getByTestId('preview-mon')
    await expect(previewMons.first()).toBeVisible({ timeout: 10_000 })
    const p1Count = await previewMons.count()
    for (let i = 0; i < p1Count; i++) await previewMons.nth(i).click()
    await page.getByTestId('preview-confirm').click()

    // ── Team preview: P2's turn, same picks, then the battle itself starts. ──
    await expect(page.getByTestId('preview-mon').first()).toBeVisible()
    const p2Count = await page.getByTestId('preview-mon').count()
    for (let i = 0; i < p2Count; i++) await page.getByTestId('preview-mon').nth(i).click()
    await page.getByTestId('preview-confirm').click()

    // ── Turn 1: singles has exactly one legal target, so the first move
    // option commits immediately without an arena target-click for either
    // player — P1 picks, the panel flips to P2 automatically (hotseat). ──────
    await expect(page.getByTestId('move-option').first()).toBeVisible({ timeout: 10_000 })
    await page.getByTestId('move-option').first().click()
    await expect(page.getByTestId('move-option').first()).toBeVisible({ timeout: 10_000 })
    await page.getByTestId('move-option').first().click()

    // Both slots committed → the turn ships and resolves; the log gains an
    // entry and the panel returns to a fresh P1 selection for turn 2.
    await expect(page.getByText('Turn 1', { exact: true })).toBeVisible({ timeout: 15_000 })
    await expect(page.getByTestId('move-option').first()).toBeVisible()
  })
})
