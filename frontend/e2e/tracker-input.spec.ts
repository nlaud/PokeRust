import { test, expect } from '@playwright/test'
import { pickSelectOption, seedTeam, startTrackerSession } from './helpers'

// Tests the tracker input against the real frontend and backend.
// It covers completion, lead input, previews, turn commits, history, deletion, and cancellation.

test.describe('Tracker input bar', () => {
  test('autocomplete, leads, two-tier commit, delete-on-empty, and navigation all work end to end', async ({
    page,
  }) => {
    await seedTeam(page)
    await startTrackerSession(page)
    const input = page.getByTestId('tracker-input')
    const lineNumber = page.getByTestId('tracker-line-number')

    // Complete a partial lead with Tab.
    // Put both sides on one `leads` line.
    await expect(lineNumber).toHaveText('1')
    await input.pressSequentially('leads p pika', { delay: 15 })
    await expect(page.getByTestId('tracker-suggestion-top')).toHaveText('Pikachu')
    // Show the remaining letters of the first suggestion after the caret.
    await expect(page.getByTestId('tracker-ghost')).toHaveText('chu')
    await page.screenshot({ path: 'e2e/screenshots/01-suggestions-and-ghost.png' })

    // Finish the word without Tab.
    // Hide suggestions after the word matches a valid candidate.
    await input.pressSequentially('chu', { delay: 15 })
    await expect(input).toHaveValue('leads p pikachu')
    await expect(page.getByTestId('tracker-suggestion-top')).toBeHidden()
    await expect(page.getByTestId('tracker-ghost')).toHaveText('')
    await page.keyboard.press('Tab')
    await expect(input).toHaveValue('leads p pikachu') // unchanged — nothing to accept

    // Add the opponent to the same line.
    await input.pressSequentially(' o garch', { delay: 15 })
    await expect(page.getByTestId('tracker-suggestion-top')).toHaveText('Garchomp')
    await page.keyboard.press('Tab')
    await expect(input).toHaveValue('leads p pikachu o Garchomp ')

    // Press Enter to save the line and show its structural preview.
    await page.keyboard.press('Enter')
    await expect(page.getByTestId('tracker-pending-turn')).toBeVisible()
    await expect(input).toHaveValue('')
    await expect(lineNumber).toHaveText('2')
    await expect(page.getByText('Pikachu', { exact: true }).first()).toBeVisible()
    await expect(page.getByText('Garchomp', { exact: true }).first()).toBeVisible()

    // Press Shift+Enter to end the turn and rebuild history.
    // Confirm the log and the new draft line.
    await page.keyboard.press('Shift+Enter')
    await expect(page.getByText('Turn 1', { exact: true })).toBeVisible()
    await expect(lineNumber).toHaveText('1')
    await page.screenshot({ path: 'e2e/screenshots/02-turn-1-committed.png' })

    // Enter a damaging move and an opponent protection move.
    // Use `thunderb` to distinguish Thunderbolt from other Thunder moves.
    await input.pressSequentially('p1 thunderb', { delay: 15 })
    await expect(page.getByTestId('tracker-suggestion-top')).toHaveText('Thunderbolt')
    await page.keyboard.press('Tab')
    await input.pressSequentially('o1 62%', { delay: 15 })
    await page.keyboard.press('Enter')
    await expect(page.getByText('62%', { exact: true }).first()).toBeVisible()
    await expect(lineNumber).toHaveText('2')

    // Clear a saved line and press Enter to remove it.
    await page.keyboard.press('ArrowUp')
    await expect(input).toHaveValue('p1 Thunderbolt o1 62%')
    await expect(lineNumber).toHaveText('1')
    await input.fill('')
    await page.keyboard.press('Enter')
    await expect(page.getByText('62%', { exact: true })).toHaveCount(0)
    await expect(input).toHaveValue('')
    // Confirm that deletion returns to the first empty line.
    await expect(lineNumber).toHaveText('1')

    // Save the line again and remove it with Backspace.
    await input.pressSequentially('p1 Thunderbolt o1 55%', { delay: 15 })
    await page.keyboard.press('Enter')
    await expect(page.getByText('55%', { exact: true }).first()).toBeVisible({ timeout: 10_000 })
    await page.keyboard.press('ArrowUp')
    await expect(input).toHaveValue('p1 Thunderbolt o1 55%')
    await input.fill('')
    await page.keyboard.press('Backspace') // buffer already empty — deletes the line
    await expect(page.getByText('55%', { exact: true })).toHaveCount(0)

    // Save the damage line and end turn 2.
    await input.pressSequentially('p1 Thunderbolt o1 55%', { delay: 15 })
    await page.keyboard.press('Enter')
    await input.pressSequentially('o1 prot', { delay: 15 })
    await expect(page.getByTestId('tracker-suggestion-top')).toHaveText('Protect')
    await page.keyboard.press('Tab')
    await page.keyboard.press('Enter')
    await page.keyboard.press('Shift+Enter')
    await expect(page.getByText('Turn 2', { exact: true })).toBeVisible()
    await expect(page.getByText('55%', { exact: true }).first()).toBeVisible()
    await page.screenshot({ path: 'e2e/screenshots/03-turn-2-committed.png' })

    // Confirm that ArrowUp does not open a line from a committed turn.
    await expect(lineNumber).toHaveText('1')
    await page.keyboard.press('ArrowUp')
    await expect(input).toHaveValue('')
    await expect(lineNumber).toHaveText('1')

    // Add one draft line and confirm that ArrowUp stops at this line.
    await input.pressSequentially('p1 prot', { delay: 15 })
    await page.keyboard.press('Tab')
    await page.keyboard.press('Enter')
    await expect(lineNumber).toHaveText('2')
    await page.keyboard.press('ArrowUp')
    await expect(input).toHaveValue('p1 Protect')
    await expect(lineNumber).toHaveText('1')
    await page.keyboard.press('ArrowUp') // already at the oldest draft line — stays put
    await expect(input).toHaveValue('p1 Protect')
    await expect(lineNumber).toHaveText('1')

    // Press Escape to discard an unsaved edit.
    await input.pressSequentially(' garbage', { delay: 15 })
    await page.keyboard.press('Escape')
    await expect(input).toHaveValue('')
    await expect(lineNumber).toHaveText('2') // back to the fresh append slot

    // Press Shift+Escape to discard the complete draft.
    await page.keyboard.press('Shift+Escape')
    await expect(input).toHaveValue('')
    await expect(lineNumber).toHaveText('1')

    // Press Shift+Escape to reopen the last committed turn.
    // Press it again to discard the reopened draft.
    await page.keyboard.press('Shift+Escape')
    await expect(lineNumber).toHaveText('3')
    await page.keyboard.press('Shift+Escape')
    await expect(input).toHaveValue('')
    await expect(lineNumber).toHaveText('1')
  })
})

test.describe('Tracker solver panel', () => {
  // The search itself runs on the server and can take tens of seconds, so this
  // test covers the panel and the request, not one complete answer.
  test('starts, restores, stops, and deletes a search with its session', async ({ page }) => {
    await seedTeam(page)
    await startTrackerSession(page)
    const panel = page.getByTestId('tracker-solver-panel')
    await expect(panel).toBeVisible()

    // The panel starts closed and reports that no search runs.
    await expect(page.getByTestId('tracker-solver-start')).toBeHidden()
    await panel.getByTestId('tracker-solver-toggle').click()
    await expect(page.getByTestId('tracker-solver-start')).toBeVisible()

    // Commit the leads so the position holds an active Pokemon on both sides.
    const input = page.getByTestId('tracker-input')
    await input.pressSequentially('leads p pikachu o Garchomp', { delay: 10 })
    await page.keyboard.press('Shift+Enter')
    await expect(page.getByText('Turn 1', { exact: true })).toBeVisible()

    await pickSelectOption(page, 'Algorithm', 'ISMCTS (sampled belief)')
    await pickSelectOption(page, 'Preset', 'Fast')
    await page.getByTestId('tracker-solver-start').click()

    // The toggle row reports the running search until the first depth answers.
    await expect(page.getByTestId('tracker-solver-stop')).toBeVisible()

    // A reload restores the profile controls from the server record.
    await page.reload()
    await expect(panel).toBeVisible()
    await panel.getByTestId('tracker-solver-toggle').click()
    await expect(
      panel.locator('label').filter({ hasText: 'Algorithm' }).getByRole('button').first(),
    ).toContainText('ISMCTS (sampled belief)')
    await expect(
      panel.locator('label').filter({ hasText: 'Preset' }).getByRole('button').first(),
    ).toContainText('Fast')

    await page.getByTestId('tracker-solver-stop').click()
    await expect(page.getByTestId('tracker-solver-stop')).toBeHidden()

    // Ending the tracker deletes its server session and cancels a new job.
    await page.getByTestId('tracker-solver-start').click()
    const deleted = page.waitForRequest(
      (request) => request.method() === 'DELETE' && /\/api\/tracker\/[^/]+$/.test(request.url()),
    )
    await page.getByRole('button', { name: 'End tracker' }).click()
    await page.getByRole('button', { name: 'Delete' }).click()
    await deleted
  })

  test('completes and commits the opening back clause', async ({ page }, testInfo) => {
    await seedTeam(page)
    await startTrackerSession(page)
    const input = page.getByTestId('tracker-input')

    await input.pressSequentially('leads p pikachu b', { delay: 15 })
    await expect(page.getByTestId('tracker-suggestion-top')).toHaveText('back')
    await expect(page.getByTestId('tracker-ghost')).toHaveText('ack')
    await page.keyboard.press('Tab')
    await expect(input).toHaveValue('leads p pikachu back ')

    await input.pressSequentially('venusaur incineroar o garchomp', { delay: 15 })
    await page.keyboard.press('Enter')
    await expect(page.getByTestId('tracker-pending-turn')).toBeVisible()
    await page.keyboard.press('Shift+Enter')
    await expect(page.getByText('Turn 1', { exact: true })).toBeVisible()
    await page.screenshot({ path: testInfo.outputPath('back-clause-committed.png') })
  })
})

// ── Casing coverage ───────────────────────────────────────────────────────
// The client and server ignore case and punctuation during name matching.
// Test completion and direct input for each supported case style.
const CASINGS: { name: string; transform: (s: string) => string }[] = [
  { name: 'lowercase', transform: (s) => s.toLowerCase() },
  { name: 'UPPERCASE', transform: (s) => s.toUpperCase() },
  { name: 'Title Case', transform: (s) => s.replace(/\b\w/g, (c) => c.toUpperCase()) },
  {
    name: 'MiXeD CaSe',
    transform: (s) =>
      s
        .split('')
        .map((c, i) => (i % 2 === 0 ? c.toLowerCase() : c.toUpperCase()))
        .join(''),
  },
]

test.describe('Tracker input bar casing', () => {
  for (const { name, transform } of CASINGS) {
    test(`autocomplete and submission both work in ${name}`, async ({ page }) => {
      await seedTeam(page)
      await startTrackerSession(page)
      const input = page.getByTestId('tracker-input')

      // Find the correct completion for each partial-word case.
      await input.pressSequentially(transform('leads p pika'), { delay: 15 })
      await expect(page.getByTestId('tracker-suggestion-top')).toHaveText('Pikachu')
      await page.keyboard.press('Tab')
      await input.pressSequentially(transform('o garch'), { delay: 15 })
      await expect(page.getByTestId('tracker-suggestion-top')).toHaveText('Garchomp')
      await page.keyboard.press('Tab')
      await page.keyboard.press('Enter')
      await page.keyboard.press('Shift+Enter')
      await expect(page.getByText('Turn 1', { exact: true })).toBeVisible()

      // Parse each case without Tab completion.
      await input.pressSequentially(transform('p1 thunderbolt o1 40%'), { delay: 15 })
      await page.keyboard.press('Enter')
      await input.pressSequentially(transform('o1 protect'), { delay: 15 })
      await page.keyboard.press('Enter')
      await page.keyboard.press('Shift+Enter')
      await expect(page.getByText('Turn 2', { exact: true })).toBeVisible()
      await expect(page.getByText('40%', { exact: true }).first()).toBeVisible()
    })
  }
})

// ── Casing matches the line so far (F2) ──────────────────────────────────────
// Later suggestions on one line use the case style of its first multiword name.
// Volt Switch sets the style, and Rough Skin checks the next suggestion.
test.describe('Tracker input bar casing continuity', () => {
  test('a snake_case move earlier on the line biases a later ability suggestion to snake_case', async ({
    page,
  }) => {
    await seedTeam(page)
    await startTrackerSession(page)
    const input = page.getByTestId('tracker-input')

    await input.pressSequentially('leads p pikachu o garchomp', { delay: 15 })
    await page.keyboard.press('Enter')
    await page.keyboard.press('Shift+Enter')
    await expect(page.getByText('Turn 1', { exact: true })).toBeVisible()

    // Enter the move in snake case.
    // Confirm that the ability suggestion also uses snake case.
    await input.pressSequentially('p1 volt_switch o1 rough_sk', { delay: 15 })
    const top = page.getByTestId('tracker-suggestion-top')
    await expect(top).toHaveText('rough_skin')
  })
})

// Tests the closed-sheet species picker with the real species endpoint.
// The picker prevents unknown species before submission.
test.describe('Tracker setup species picker', () => {
  test('filters, autocorrects, pastes, removes, and gates the start button', async ({ page }) => {
    await seedTeam(page)
    await page.goto('/tracker')
    await pickSelectOption(page, 'Ruleset', 'Pokémon Champions Season 2 Singles')

    const species = page.getByTestId('species-input')
    const start = page.getByRole('button', { name: 'Start Tracking' })
    const chips = page.getByTestId('species-chip')
    const suggestions = page.getByTestId('species-suggestion')

    // Keep submission disabled until the user selects three species.
    await expect(start).toBeDisabled()
    await expect(page.getByText('0 added — need at least 3')).toBeVisible()

    // Sort species prefix matches alphabetically.
    await species.pressSequentially('garch', { delay: 15 })
    await expect(suggestions.first()).toHaveText('Garchomp')
    await species.press('Enter')
    await expect(chips).toHaveCount(1)
    await expect(chips.first()).toContainText('Garchomp')

    // Show a close species match after a typing error.
    await species.pressSequentially('toxapx', { delay: 15 })
    await expect(suggestions.first()).toHaveText('Toxapex')
    await species.press('Tab')
    await expect(chips).toHaveCount(2)

    // Do not show battle-only or Mega forms.
    await species.pressSequentially('garchompmeg', { delay: 15 })
    await expect(suggestions.filter({ hasText: 'Garchomp Mega' })).toHaveCount(0)
    await species.press('Escape')

    // Keep submission disabled with only two species.
    await expect(start).toBeDisabled()

    // Remove the last species with Backspace.
    // Then add species from comma-delimited text.
    await species.press('Backspace')
    await expect(chips).toHaveCount(1)

    await species.pressSequentially('toxapex', { delay: 15 })
    await species.press('Enter')
    await species.pressSequentially('rotomwash', { delay: 15 })
    await species.press('Enter')
    await expect(chips).toHaveCount(3)
    await expect(start).toBeEnabled()

    // Remove one species with its button and disable submission.
    await chips.first().getByRole('button').click()
    await expect(chips).toHaveCount(2)
    await expect(start).toBeDisabled()
  })
})
