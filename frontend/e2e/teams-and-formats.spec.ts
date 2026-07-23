import { test, expect } from '@playwright/test'

// Smoke coverage for the two localStorage-backed CRUD pages (Teams, Formats)
// — no server involved at all, these never leave the browser.

test.describe('Teams page', () => {
  test('creating a team persists it as a card', async ({ page }) => {
    await page.goto('/')
    await page.getByRole('button', { name: 'Add team' }).click()
    await page.getByPlaceholder('Team name').fill('E2E Test Team')
    await page.getByPlaceholder(/Paste a Showdown teamsheet/).fill('Pikachu\nLevel: 50\n- Thunderbolt')
    await page.getByRole('button', { name: 'Save' }).click()

    await expect(page.getByText('E2E Test Team', { exact: true })).toBeVisible()

    // Reload — the whole point of localStorage persistence is surviving one.
    await page.reload()
    await expect(page.getByText('E2E Test Team', { exact: true })).toBeVisible()
  })
})

test.describe('Formats page', () => {
  test('creating a format persists it with the right brought/active summary', async ({ page }) => {
    await page.goto('/formats')
    await page.getByRole('button', { name: 'Add format' }).click()
    await page.getByPlaceholder('Format name').fill('E2E Test Format')

    // Active / Brought / Total number inputs, in that DOM order (see
    // `FormatEditor`'s `[field, label]` table). Picked to not collide with
    // either seeded default format's own active/brought/total (Doubles is
    // 2/4/6, Singles is 1/3/6) — a collision would make the summary text
    // assertion below match two cards instead of one.
    const numberInputs = page.locator('input[type="number"]')
    await numberInputs.nth(0).fill('1')
    await numberInputs.nth(1).fill('2')
    await numberInputs.nth(2).fill('4')

    await page.getByRole('button', { name: 'Save' }).click()

    await expect(page.getByText('E2E Test Format', { exact: true })).toBeVisible()
    await expect(page.getByText('1 active / bring 2 of 4')).toBeVisible()
  })
})
