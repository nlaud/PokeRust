import { test, expect } from '@playwright/test'
import { seedTeam, startTrackerSession } from './helpers'

// End-to-end coverage for the tracker mode input bar (TrackerInputBar.tsx):
// autocomplete/ghost-text/suggestion ranking, the two-tier commit model
// (per-event structural preview via Enter, per-turn full rebuild via
// Shift+Enter), editing an already-committed event and watching the belief
// recompute, and the Escape/Shift+Escape navigation contract. Drives the
// REAL server (release build) + Vite dev server, both launched by
// playwright.config.ts's webServer array — no mocking.

test.describe('Tracker input bar', () => {
  test('autocomplete, two-tier commit, editing a past event, and navigation all work end to end', async ({
    page,
  }) => {
    await seedTeam(page)
    await startTrackerSession(page)
    const input = page.getByTestId('tracker-input')

    // ── 1. Autocomplete: partial word ranks a top suggestion, ghost text
    // shows its remainder, Tab accepts it. ──────────────────────────────
    await input.pressSequentially('p leads pika', { delay: 15 })
    await expect(page.getByTestId('tracker-suggestion-top')).toHaveText('Pikachu')
    // The top suggestion's remainder past what's typed ("Pikachu" minus the
    // typed "pika") renders as ghost text right after the caret.
    await expect(page.getByTestId('tracker-ghost')).toHaveText('chu')
    await page.screenshot({ path: 'e2e/screenshots/01-suggestions-and-ghost.png' })
    await page.keyboard.press('Tab')
    await expect(input).toHaveValue('p leads Pikachu ')

    // ── 2. Enter commits the line locally (triggering the per-event
    // structural preview) and jumps to a fresh event — a SEPARATE line for
    // the opponent's leads, not appended onto the same one. ────────────────
    await page.keyboard.press('Enter')
    await expect(page.getByTestId('tracker-pending-turn')).toBeVisible()
    await expect(input).toHaveValue('')

    await input.pressSequentially('o leads garch', { delay: 15 })
    await expect(page.getByTestId('tracker-suggestion-top')).toHaveText('Garchomp')
    await page.keyboard.press('Tab')
    await expect(input).toHaveValue('o leads Garchomp ')
    await page.keyboard.press('Enter')
    await expect(page.getByText('Pikachu', { exact: true }).first()).toBeVisible()
    await expect(page.getByText('Garchomp', { exact: true }).first()).toBeVisible()

    // ── 3. Shift+Enter ends the turn: appends `endofturn`, rebuilds through
    // `PUT /history`, and the committed log gains "Turn 1". ─────────────────
    await page.keyboard.press('Shift+Enter')
    await expect(page.getByText('Turn 1', { exact: true })).toBeVisible()
    await page.screenshot({ path: 'e2e/screenshots/02-turn-1-committed.png' })

    // ── Turn 2: a damaging move, move-line effect-vocabulary completion
    // (status/volatile/stat words share the same completion path as names),
    // then a Protect from the opponent. ─────────────────────────────────
    // "thunderb" (not just "thunder") to disambiguate from "Thunder"/"Thunder
    // Fang"/etc., which some roster mon may also know — prefix ranking alone
    // doesn't prefer the shorter or the longer match, so over-typing here
    // keeps the assertion meaningful regardless of what else is in the pool.
    await input.pressSequentially('p1 thunderb', { delay: 15 })
    await expect(page.getByTestId('tracker-suggestion-top')).toHaveText('Thunderbolt')
    await page.keyboard.press('Tab')
    await input.pressSequentially('o1 62%', { delay: 15 })
    await page.keyboard.press('Enter')
    await expect(page.getByText('62%', { exact: true }).first()).toBeVisible()

    await input.pressSequentially('o1 prot', { delay: 15 })
    await expect(page.getByTestId('tracker-suggestion-top')).toHaveText('Protect')
    await page.keyboard.press('Tab')
    await page.keyboard.press('Enter')
    await page.keyboard.press('Shift+Enter')
    await expect(page.getByText('Turn 2', { exact: true })).toBeVisible()

    // ── 4. Edit a PAST event: ArrowUp twice from the fresh append position
    // lands on turn 2's Thunderbolt line; retype the damage and save. This
    // must trigger a full rebuild — the belief/HP should visibly recompute. ──
    await page.keyboard.press('ArrowUp')
    await page.keyboard.press('ArrowUp')
    await expect(input).toHaveValue('p1 Thunderbolt o1 62%')
    await input.fill('')
    await input.pressSequentially('p1 Thunderbolt o1 55%', { delay: 15 })
    await page.keyboard.press('Enter')
    await expect(page.getByText('55%', { exact: true }).first()).toBeVisible({ timeout: 10_000 })
    await expect(page.getByText('62%', { exact: true })).toHaveCount(0)
    await page.screenshot({ path: 'e2e/screenshots/03-edited-past-event-recomputed.png' })

    // ── 5. Escape discards an unsaved edit without changing content. ────────
    await page.keyboard.press('ArrowUp') // load the last committed line ("o1 Protect")
    await input.pressSequentially(' garbage', { delay: 15 })
    await page.keyboard.press('Escape')
    await expect(input).toHaveValue('')

    // ── 6. Shift+Escape discards the (empty) in-progress draft and reopens
    // the last COMMITTED line for editing — no network round trip needed
    // since nothing uncommitted existed to discard. ─────────────────────────
    await page.keyboard.press('Shift+Escape')
    await expect(input).toHaveValue('o1 Protect')
  })
})

// ── Casing coverage ───────────────────────────────────────────────────────
// The grammar is case/punctuation-insensitive end to end — `tracker_parse.rs`'s
// `norm()` on the server, mirrored by `lib/trackerGrammar.ts`'s `norm()` for
// completion ranking. Prove both layers hold for every casing style a real
// user might type in, not just the lowercase the main flow above happens to
// use: autocomplete must still rank/Tab-fill correctly, AND a fully-typed
// (never-autocompleted) line in that casing must still parse and commit
// through the real server grammar.
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

      // Autocomplete ranking must find the right candidate regardless of the
      // partial word's casing.
      await input.pressSequentially(transform('p leads pika'), { delay: 15 })
      await expect(page.getByTestId('tracker-suggestion-top')).toHaveText('Pikachu')
      await page.keyboard.press('Tab')
      await page.keyboard.press('Enter')

      await input.pressSequentially(transform('o leads garch'), { delay: 15 })
      await expect(page.getByTestId('tracker-suggestion-top')).toHaveText('Garchomp')
      await page.keyboard.press('Tab')
      await page.keyboard.press('Enter')
      await page.keyboard.press('Shift+Enter')
      await expect(page.getByText('Turn 1', { exact: true })).toBeVisible()

      // A fully hand-typed line (no Tab completion at all) in this casing
      // must still resolve through the real server-side parser.
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
