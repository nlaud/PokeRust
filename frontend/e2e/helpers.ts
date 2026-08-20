import { expect, type Page } from '@playwright/test'

// Provides shared end-to-end fixtures and helpers.
// Each test uses the same local-storage schema.

// Singles requires three selected Pokémon on each side.
// Most tests send out only Pikachu.
// Each species in this shared team needs a Champions learnset entry.
// The P2 bot draws Player 1's hidden moves from that dex.
export const TEAM_SHEET = `Pikachu
Ability: Static
Level: 50
EVs: 252 HP / 4 Atk / 252 Spe
Jolly Nature
- Thunderbolt
- Protect
- Volt Switch
- Iron Tail

Venusaur
Level: 50
- Power Whip

Incineroar
Level: 50
- Flare Blitz`

/** Stores one team for the simulator and tracker setup panels. */
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

/** Selects a visible option from the custom list box for the specified field. */
export async function pickSelectOption(page: Page, labelText: string, optionName: string) {
  // Scoped to the field, not to the page. `Select` renders its trigger and its
  // list inside one root, and the settings sidebar stays mounted on every page,
  // so two fields can offer an option of the same name at the same time.
  const field = page.locator(`label:has-text("${labelText}")`)
  await field.locator('button[aria-haspopup="listbox"]').click()
  await field.getByRole('option', { name: optionName }).click()
}

/** Picks the solver search in the settings sidebar, then closes the sidebar.
 *
 * The picker lives beside the other solver limits, so every page that runs a
 * search reads the same choice. */
export async function pickSolverSearch(page: Page, optionName: string) {
  await page.getByRole('button', { name: 'Open settings' }).click()
  await pickSelectOption(page, 'Search', optionName)
  await page.getByRole('button', { name: 'Close settings' }).click()
}

export async function startTrackerSession(page: Page) {
  await page.goto('/tracker')
  // Use Singles to keep tracker turn history simple.
  await pickSelectOption(page, 'Ruleset', 'Pokémon Champions Season 2 Singles')
  // Add three opponent species because Singles requires three.
  // The test sends out only Garchomp.
  // The closed-sheet picker validates each species against the server catalog.
  const species = page.getByTestId('species-input')
  for (const name of ['Garchomp', 'Toxapex', 'Rotom-Wash']) {
    await species.pressSequentially(name, { delay: 15 })
    await expect(page.getByTestId('species-suggestion').first()).toBeVisible()
    await species.press('Enter')
  }
  await expect(page.getByTestId('species-chip')).toHaveCount(3)
  await page.getByRole('button', { name: 'Start Tracking' }).click()
  await expect(page.getByTestId('tracker-input')).toBeVisible({ timeout: 10_000 })
}
