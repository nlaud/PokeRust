import { expect, type Page } from '@playwright/test'

// Shared fixtures/helpers across the e2e suite (tracker, simulate, teams,
// formats). Kept in one place so every spec seeds the exact same localStorage
// shape a real user's browser would have — see `lib/storage.ts`'s schemas.

// Singles brings 3 Pokemon per side (see `DEFAULT_FORMATS` in
// `lib/storage.ts`) — the roster needs at least that many or the server
// rejects it with "N Pokemon parsed but the format brings 3". Only Pikachu is
// ever actually sent out in most specs; the other two exist solely to satisfy
// that count.
export const TEAM_SHEET = `Pikachu
Ability: Static
Level: 50
EVs: 252 HP / 4 Atk / 252 Spe
Jolly Nature
- Thunderbolt
- Protect
- Volt Switch
- Iron Tail

Ferrothorn
Level: 50
- Power Whip

Incineroar
Level: 50
- Flare Blitz`

/** Seed exactly the localStorage a real user would have after building one
 * team on the Teams page — both `TrackerSetupPanel` and `SetupPanel` (battle
 * mode) require at least one stored team to be selectable at all. */
export async function seedTeam(page: Page) {
  await page.addInitScript(
    ({ sheet }) => {
      localStorage.setItem(
        'pokerust.teams.v1',
        JSON.stringify({
          teams: [
            { id: 'team-1', name: 'Test Team', sheet, favorite: false, updatedAt: new Date().toISOString() },
          ],
        }),
      )
    },
    { sheet: TEAM_SHEET },
  )
}

/** Selects an option from the app's custom `Select` component (a listbox
 * button, not a native `<select>` — see `components/common/Select.tsx`).
 * `labelText` is the field's own `<label>` text (e.g. "Ruleset"); `optionName`
 * is the option's visible text. */
export async function pickSelectOption(page: Page, labelText: string, optionName: string) {
  await page.locator(`label:has-text("${labelText}") button[aria-haspopup="listbox"]`).click()
  await page.getByRole('option', { name: optionName }).click()
}

export async function startTrackerSession(page: Page) {
  await page.goto('/tracker')
  // Force Singles (1v1) so flat-history/turn math in the tracker specs is
  // simple to reason about — the default format is Doubles otherwise.
  await pickSelectOption(page, 'Ruleset', 'Pokémon Champions Season 2 Singles')
  // Same 3-brought minimum applies to the opponent side; only Garchomp is
  // ever actually sent out.
  await page.locator('textarea').fill('Garchomp, Toxapex, Rotom-Wash')
  await page.getByRole('button', { name: 'Start Tracking' }).click()
  await expect(page.getByTestId('tracker-input')).toBeVisible({ timeout: 10_000 })
}
