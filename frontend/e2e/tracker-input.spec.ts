import { test, expect } from '@playwright/test'
import { pickSelectOption, seedTeam, startTrackerSession } from './helpers'

// End-to-end coverage for the tracker mode input bar (TrackerInputBar.tsx):
// autocomplete/ghost-text/suggestion ranking (including suppression once a
// word is already complete), the combined `leads` grammar, the two-tier
// commit model (per-event structural preview via Enter, per-turn full
// rebuild via Shift+Enter), turn-scoped history navigation with the
// line-number gutter, delete-on-empty for Enter/Backspace, and the
// Escape/Shift+Escape contract. Drives the REAL server (release build) +
// Vite dev server, both launched by playwright.config.ts's webServer array —
// no mocking.

test.describe('Tracker input bar', () => {
  test('autocomplete, leads, two-tier commit, delete-on-empty, and navigation all work end to end', async ({
    page,
  }) => {
    await seedTeam(page)
    await startTrackerSession(page)
    const input = page.getByTestId('tracker-input')
    const lineNumber = page.getByTestId('tracker-line-number')

    // ── 1. Autocomplete: partial word ranks a top suggestion, ghost text
    // shows its remainder, Tab accepts it. The combined `leads` line covers
    // both sides on ONE line (`leads p <species> o <species>`). ────────────
    await expect(lineNumber).toHaveText('1')
    await input.pressSequentially('leads p pika', { delay: 15 })
    await expect(page.getByTestId('tracker-suggestion-top')).toHaveText('Pikachu')
    // The top suggestion's remainder past what's typed ("Pikachu" minus the
    // typed "pika") renders as ghost text right after the caret.
    await expect(page.getByTestId('tracker-ghost')).toHaveText('chu')
    await page.screenshot({ path: 'e2e/screenshots/01-suggestions-and-ghost.png' })

    // ── 2. Finish the word BY HAND (no Tab) instead of accepting the
    // suggestion — once the typed word already exactly matches a valid
    // candidate (case/punctuation-insensitively), the ghost text and the
    // whole suggestion panel disappear (F4): there's nothing left to
    // autocomplete, and no reason to keep dangling an autocorrect option away
    // from what was just typed. Tab is likewise a no-op here (nothing to
    // accept). ───────────────────────────────────────────────────────────────
    await input.pressSequentially('chu', { delay: 15 })
    await expect(input).toHaveValue('leads p pikachu')
    await expect(page.getByTestId('tracker-suggestion-top')).toBeHidden()
    await expect(page.getByTestId('tracker-ghost')).toHaveText('')
    await page.keyboard.press('Tab')
    await expect(input).toHaveValue('leads p pikachu') // unchanged — nothing to accept

    // Continue the SAME line with the opponent's side.
    await input.pressSequentially(' o garch', { delay: 15 })
    await expect(page.getByTestId('tracker-suggestion-top')).toHaveText('Garchomp')
    await page.keyboard.press('Tab')
    await expect(input).toHaveValue('leads p pikachu o Garchomp ')

    // ── 3. Enter commits the line locally (triggering the per-event
    // structural preview) and jumps to a fresh event, advancing the
    // line-number gutter. ────────────────────────────────────────────────────
    await page.keyboard.press('Enter')
    await expect(page.getByTestId('tracker-pending-turn')).toBeVisible()
    await expect(input).toHaveValue('')
    await expect(lineNumber).toHaveText('2')
    await expect(page.getByText('Pikachu', { exact: true }).first()).toBeVisible()
    await expect(page.getByText('Garchomp', { exact: true }).first()).toBeVisible()

    // ── 4. Shift+Enter ends the turn: appends `endofturn`, rebuilds through
    // `PUT /history`, and the committed log gains "Turn 1". The gutter resets
    // to line 1 of the new (empty) turn 2 draft. ────────────────────────────
    await page.keyboard.press('Shift+Enter')
    await expect(page.getByText('Turn 1', { exact: true })).toBeVisible()
    await expect(lineNumber).toHaveText('1')
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
    await expect(lineNumber).toHaveText('2')

    // ── 5. Delete-on-empty (F6): clearing the just-saved line and pressing
    // Enter (or Backspace) removes that event instead of saving an empty one,
    // stepping back to the previous line. ───────────────────────────────────
    await page.keyboard.press('ArrowUp')
    await expect(input).toHaveValue('p1 Thunderbolt o1 62%')
    await expect(lineNumber).toHaveText('1')
    await input.fill('')
    await page.keyboard.press('Enter')
    await expect(page.getByText('62%', { exact: true })).toHaveCount(0)
    await expect(input).toHaveValue('')
    // The line was deleted — the gutter is back to a fresh line 1 (this
    // draft is now empty again).
    await expect(lineNumber).toHaveText('1')

    // Retype it, save, then delete it again via Backspace this time.
    await input.pressSequentially('p1 Thunderbolt o1 55%', { delay: 15 })
    await page.keyboard.press('Enter')
    await expect(page.getByText('55%', { exact: true }).first()).toBeVisible({ timeout: 10_000 })
    await page.keyboard.press('ArrowUp')
    await expect(input).toHaveValue('p1 Thunderbolt o1 55%')
    await input.fill('')
    await page.keyboard.press('Backspace') // buffer already empty — deletes the line
    await expect(page.getByText('55%', { exact: true })).toHaveCount(0)

    // ── 6. Retype the damage line for real and end turn 2. ──────────────────
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

    // ── 7. ArrowUp is scoped to the CURRENT turn's draft only — it must NOT
    // walk back into turn 1 or turn 2's already-committed lines (the
    // previous behavior "up-arrows into the previous turn" bug fix). With
    // turn 3's draft still empty, ArrowUp is a no-op at the append position. ──
    await expect(lineNumber).toHaveText('1')
    await page.keyboard.press('ArrowUp')
    await expect(input).toHaveValue('')
    await expect(lineNumber).toHaveText('1')

    // Type one line into turn 3's draft, then confirm ArrowUp stops at THIS
    // line — it never surfaces turn 2's "o1 Protect" or earlier content.
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

    // ── 8. Escape discards an unsaved edit without changing content. ────────
    await input.pressSequentially(' garbage', { delay: 15 })
    await page.keyboard.press('Escape')
    await expect(input).toHaveValue('')
    await expect(lineNumber).toHaveText('2') // back to the fresh append slot

    // ── 9. Shift+Escape discards the WHOLE in-progress draft (both saved
    // draft lines from this turn) and starts the turn over from empty. ──────
    await page.keyboard.press('Shift+Escape')
    await expect(input).toHaveValue('')
    await expect(lineNumber).toHaveText('1')
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
      await input.pressSequentially(transform('leads p pika'), { delay: 15 })
      await expect(page.getByTestId('tracker-suggestion-top')).toHaveText('Pikachu')
      await page.keyboard.press('Tab')
      await input.pressSequentially(transform('o garch'), { delay: 15 })
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

// ── Casing matches the line so far (F2) ──────────────────────────────────────
// Once a multi-word name has been typed in a particular whitespace-free
// casing, later multi-word suggestions on the SAME line should offer that
// same casing rather than always defaulting to PascalCase. "Volt Switch" is
// one of Pikachu's own known moves (see `TEAM_SHEET` in `helpers.ts`) — a
// genuinely two-word name usable to set the style; "Rough Skin" is one of
// Garchomp's possible abilities (in the match's ability pool regardless of
// which ability the opponent actually has, since completions are scoped to
// every roster species' full possible-ability list, not the revealed one).
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

    // Type the move in explicit snake_case (completing it, so it becomes a
    // finished token the style-detector can inspect), target the opponent,
    // then start an ability-reveal word on the SAME line and check the
    // suggestion offered matches the snake_case style already established.
    await input.pressSequentially('p1 volt_switch o1 rough_sk', { delay: 15 })
    const top = page.getByTestId('tracker-suggestion-top')
    await expect(top).toHaveText('rough_skin')
  })
})

// The tracker SETUP form's opponent field under Closed Team Sheet
// (`SpeciesPicker.tsx`). Replaces the old free-text textarea, whose failure
// mode was silent: the teamsheet parser drops an unrecognized species and the
// only feedback is a generic "N Pokemon parsed but the format brings M" 422.
// Backed by the real `GET /api/dex/species` catalog — no mocking.
test.describe('Tracker setup species picker', () => {
  test('filters, autocorrects, pastes, removes, and gates the start button', async ({ page }) => {
    await seedTeam(page)
    await page.goto('/tracker')
    await pickSelectOption(page, 'Ruleset', 'Pokémon Champions Season 2 Singles')

    const species = page.getByTestId('species-input')
    const start = page.getByRole('button', { name: 'Start Tracking' })
    const chips = page.getByTestId('species-chip')
    const suggestions = page.getByTestId('species-suggestion')

    // Nothing picked yet — Singles brings 3, so the button stays disabled.
    await expect(start).toBeDisabled()
    await expect(page.getByText('0 added — need at least 3')).toBeVisible()

    // Prefix match, alphabetically ordered (unlike the tracker input bar's
    // deliberately-scrambled stable-hash ranking, which suits small keyword
    // pools but not a thousand-entry species list).
    await species.pressSequentially('garch', { delay: 15 })
    await expect(suggestions.first()).toHaveText('Garchomp')
    await species.press('Enter')
    await expect(chips).toHaveCount(1)
    await expect(chips.first()).toContainText('Garchomp')

    // Edit-distance autocorrect: a typo still surfaces the intended species
    // instead of an empty list.
    await species.pressSequentially('toxapx', { delay: 15 })
    await expect(suggestions.first()).toHaveText('Toxapex')
    await species.press('Tab')
    await expect(chips).toHaveCount(2)

    // Battle-only and Mega formes are filtered server-side — you can never
    // put "Garchomp-Mega" on a teamsheet, so it must not be offered.
    await species.pressSequentially('garchompmeg', { delay: 15 })
    await expect(suggestions.filter({ hasText: 'Garchomp Mega' })).toHaveCount(0)
    await species.press('Escape')

    // Still one short.
    await expect(start).toBeDisabled()

    // Backspace on an empty query removes the last chip (standard chip-input
    // behaviour), then re-add via paste of a comma-separated list.
    await species.press('Backspace')
    await expect(chips).toHaveCount(1)

    await species.pressSequentially('toxapex', { delay: 15 })
    await species.press('Enter')
    await species.pressSequentially('rotomwash', { delay: 15 })
    await species.press('Enter')
    await expect(chips).toHaveCount(3)
    await expect(start).toBeEnabled()

    // The × on a chip removes just that one and re-disables the button.
    await chips.first().getByRole('button').click()
    await expect(chips).toHaveCount(2)
    await expect(start).toBeDisabled()
  })
})
