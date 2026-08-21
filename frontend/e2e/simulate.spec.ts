import { test, expect } from '@playwright/test'
import { pickSelectOption, pickSolverSearch, seedTeam } from './helpers'

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

  test('the P2 solver picker resolves an honest profile badge', async ({ page }, testInfo) => {
    await seedTeam(page)
    await page.goto('/simulate')
    await pickSelectOption(page, 'Ruleset', 'Pokémon Champions Season 2 Singles')
    const turnRequests: Record<string, unknown>[] = []
    let analysisRequests = 0
    page.on('request', (request) => {
      if (request.url().endsWith('/analysis')) analysisRequests += 1
      if (request.url().endsWith('/turn')) {
        turnRequests.push(request.postDataJSON() as Record<string, unknown>)
      }
    })

    // Keep this search small while the test checks the resolved raw limits.
    await page.route('**/api/battles', async (route) => {
      const request = route.request().postDataJSON()
      request.botP2.simulationTurnBudget = 1000
      request.botP2.depth = 1
      request.botP2.replacementDepth = 2
      request.botP2.particles = 2
      await route.continue({ postData: JSON.stringify(request) })
    })
    let holdPreviewAnalysis = true
    await page.route('**/api/battles/*/analysis', async (route) => {
      if (holdPreviewAnalysis) {
        await new Promise((resolve) => setTimeout(resolve, 500))
      }
      await route.continue()
    })

    // The profile section uses a span label. Scope each list box by its test ID.
    const picker = page.getByTestId('bot-picker')

    // The resolved search appears only after the user selects a profile.
    await expect(picker.getByRole('button', { name: 'None' })).toBeVisible()
    await picker.getByRole('button', { name: 'None' }).click()
    await picker.getByRole('option', { name: 'Solver', exact: true }).click()
    // The picker holds no algorithm list. The information mode picks the
    // category, and the Settings sidebar picks the search inside it, so the two
    // can never disagree. The default mode hides data, so the battle reads the
    // imperfect-information search.
    await expect(picker.getByTestId('bot-algorithm-name')).toHaveText('ISMCTS (sampled belief)')
    await expect(picker).toContainText('P2 uses the balanced limits from Settings')

    const hint = picker.getByTestId('bot-algorithm-hint')
    const limit = picker.getByTestId('bot-algorithm-limit')
    await expect(hint).toContainText('Sampled')
    await expect(hint).toContainText('respects the fog of war')
    await expect(limit).toContainText('only a belief search can control P2')
    await expect(limit).toContainText('imperfect-information search from Settings')

    // The other dropdown belongs to the other category, so changing it cannot
    // reach a fog-of-war battle.
    await pickSolverSearch(page, 'perfect', 'MCTS (sampled)')
    await expect(picker.getByTestId('bot-algorithm-name')).toHaveText('ISMCTS (sampled belief)')

    // Changing the search that this mode does read reaches the picker at once.
    await pickSolverSearch(page, 'imperfect', 'MCCFR (sampled belief)')
    await expect(picker.getByTestId('bot-algorithm-name')).toHaveText('MCCFR (sampled belief)')
    await page.screenshot({
      path: testInfo.outputPath('bot-picker-resolved.png'),
      fullPage: true,
      animations: 'disabled',
    })
    // The rest of this test reads an ismcts badge, so put that search back.
    await pickSolverSearch(page, 'imperfect', 'ISMCTS (sampled belief)')

    // The picker holds no limit of its own. The Settings sidebar owns every
    // limit, and the picker names the preset that P2 will use. A preset change
    // must therefore reach this line.
    await expect(picker.locator('input[type=number]')).toHaveCount(0)
    await page.getByRole('button', { name: 'Open settings' }).click()
    await page.getByRole('button', { name: 'Fast 10K turns' }).click()
    await page.getByRole('button', { name: 'Close settings' }).click()
    await expect(picker).toContainText('P2 uses the fast limits from Settings')

    // Put the balanced preset back, so the badge below reports the limits that
    // the rest of this test expects.
    await page.getByRole('button', { name: 'Open settings' }).click()
    await page.getByRole('button', { name: 'Balanced 100K turns' }).click()
    await page.getByRole('button', { name: 'Close settings' }).click()
    await expect(picker).toContainText('P2 uses the balanced limits from Settings')
    await page.screenshot({
      path: testInfo.outputPath('bot-picker.png'),
      fullPage: true,
      animations: 'disabled',
    })

    await page.getByRole('button', { name: 'Start Battle' }).click()

    // The card names the resolved algorithm and raw limits.
    const badge = page.getByTestId('bot-badge')
    await expect(badge).toBeVisible({ timeout: 10_000 })
    await expect(badge).toContainText('ISMCTS')

    // The new badge must not make a narrow viewport overflow horizontally.
    await page.setViewportSize({ width: 390, height: 844 })
    const overflowing = await badge.evaluate((root) => {
      const bounds = root.getBoundingClientRect()
      return [...root.querySelectorAll<HTMLElement>('*')]
        .filter((element) => {
          if (element.getClientRects().length === 0) return false
          const rect = element.getBoundingClientRect()
          return rect.left < bounds.left - 1 || rect.right > bounds.right + 1
        })
        .map((element) => ({
          tag: element.tagName,
          testId: element.dataset.testid,
          className: element.className,
          text: element.textContent?.trim().slice(0, 80),
          right: element.getBoundingClientRect().right,
        }))
    })
    expect(overflowing).toEqual([])

    // The card shows approximations only while their label has hover or focus.
    const approximations = page.getByTestId('solver-approximations')
    await approximations.getByRole('button').hover()
    await expect(approximations).toContainText('samples trajectories')
    await expect(approximations).toContainText('world(s) from the belief')

    // The expanded part keeps the profile and the result in this card.
    await badge.getByRole('button').first().click()
    const detail = page.getByTestId('bot-badge-detail')
    await expect(detail).toContainText('depth 1')
    await expect(detail).toContainText('replacement depth 2')
    await expect(detail).toContainText('1,000 simulation turns')
    await expect(detail).toContainText('1 damage rolls')
    const detailOwnsOverlap = await page.evaluate(() => {
      const detail = document.querySelector<HTMLElement>('[data-testid="bot-badge-detail"]')
      const controls = document.querySelector<HTMLElement>('[data-testid="preview-confirm"]')
      if (!detail || !controls) return false
      const detailRect = detail.getBoundingClientRect()
      const controlRect = controls.getBoundingClientRect()
      const left = Math.max(detailRect.left, controlRect.left)
      const right = Math.min(detailRect.right, controlRect.right)
      const top = Math.max(detailRect.top, controlRect.top)
      const bottom = Math.min(detailRect.bottom, controlRect.bottom)
      if (left >= right || top >= bottom) return true
      const topElement = document.elementFromPoint((left + right) / 2, (top + bottom) / 2)
      return !!topElement && detail.contains(topElement)
    })
    expect(detailOwnsOverlap).toBe(true)

    // The open panel covers the controls below it, so close it before the
    // battle input starts.
    await badge.getByRole('button').first().click()
    await expect(detail).toHaveCount(0)
    await page.setViewportSize({ width: 1280, height: 800 })

    // P1 locks the preview command. The client locks all input during the wait.
    const previewMons = page.getByTestId('preview-mon')
    const p1Count = await previewMons.count()
    for (let i = 0; i < p1Count; i++) await previewMons.nth(i).click()
    await expect(page.getByTestId('preview-confirm')).toHaveText('Start Battle')
    await page.getByTestId('preview-confirm').click()
    await expect(page.getByTestId('bot-wait-line')).toBeVisible()
    await expect(page.getByTestId('bot-wait-finish')).toHaveText('Choose current move')
    await expect(page.getByTestId('bot-wait-cancel')).toHaveText('Change my selection')
    for (let i = 0; i < p1Count; i++) await expect(previewMons.nth(i)).toBeDisabled()
    await expect(page.getByRole('button', { name: 'Back', exact: false })).toBeDisabled()
    await expect(page.getByRole('button', { name: 'New battle' })).toBeDisabled()
    await page.screenshot({
      path: testInfo.outputPath('bot-preview-input-locked.png'),
      fullPage: true,
      animations: 'disabled',
    })
    // The duration is an estimate, so end the search through the explicit
    // finish action and use its current complete strategy.
    holdPreviewAnalysis = false
    await page.getByTestId('bot-wait-finish').click()
    await expect(page.getByTestId('move-option').first()).toBeVisible({ timeout: 10_000 })
    expect(turnRequests).toHaveLength(1)
    expect(turnRequests[0]).not.toHaveProperty('p2')
    // Team preview draws no command, so the reveal panel stays away.
    await expect(page.getByTestId('p2-reveal')).toHaveCount(0)
    await page.screenshot({
      path: testInfo.outputPath('bot-preview-resolved.png'),
      fullPage: true,
      animations: 'disabled',
    })

    // Watch for the terminal response before P1 submits a move.
    // This prevents the test from missing the last polling response.
    const terminalAnalysis = page.waitForResponse(async (response) => {
      if (!response.url().endsWith('/analysis')) return false
      const body = (await response.json()) as { phase?: string }
      return body.phase === 'complete' || body.phase === 'failed'
    })

    // P1 locks one move. The client waits for analysis and never asks for P2 input.
    await page.getByTestId('move-option').first().click()
    // The turn can end in a replacement, so assert the log, not a move list.
    const [analysisResponse] = await Promise.all([
      terminalAnalysis,
      expect(page.getByText('Turn 1', { exact: true })).toBeVisible({ timeout: 15_000 }),
    ])
    expect(analysisRequests).toBeGreaterThan(0)
    const analysis = (await analysisResponse.json()) as {
      generation: number
      phase: string
      checkpoint: { generation: number; stale: boolean } | null
      error: string | null
    }
    expect(analysis).toMatchObject({
      phase: 'complete',
      checkpoint: { generation: analysis.generation, stale: false },
      error: null,
    })
    expect(turnRequests).toHaveLength(2)
    expect(turnRequests[1]).not.toHaveProperty('p2')

    // A bot session always reveals P2's strategy. `reveals_strategy` in
    // `poke_rust/src/bin/server/analysis.rs` reads the profile alone, so the
    // rows appear beside the drawn action.
    await badge.getByRole('button').first().click()
    const reveal = page.getByTestId('p2-reveal')
    await expect(reveal).toBeVisible()
    await expect(reveal).toContainText('Player 2 played')
    await expect(reveal).toContainText('from the solver strategy')
    await expect(reveal).toContainText('Draw seed')
    await expect(reveal).toContainText('ismcts')
    await expect(reveal).toContainText('replacement depth 2')
    await expect(reveal).toContainText('Strategy of the last draw')
    // The rows carry action rates. They must never carry P2's win probability,
    // which would tell P1 how the solver reads the position.
    await expect(reveal).not.toContainText(/win/i)
    await page.screenshot({
      path: testInfo.outputPath('bot-turn-resolved.png'),
      fullPage: true,
      animations: 'disabled',
    })
  })

  test('the information mode selects which stored search plays P2', async ({ page }) => {
    await page.goto('/')
    await page.evaluate(() => {
      localStorage.setItem(
        'pokerust.battleSetup.v1',
        JSON.stringify({
          formatId: 'champions-s2-doubles',
          team1Id: '',
          team2Id: '',
          informationMode: 'closedSheet',
          botPreset: 'fast',
          // An older build stored the search on the setup. Settings owns it
          // now, so this field must not reach the picker.
          botAlgorithm: 'doubleOracle',
        }),
      )
      // One earlier build kept a single search across both categories. The
      // load path reads it into whichever list holds it.
      localStorage.setItem(
        'pokerust.settings.v2',
        JSON.stringify({ theme: 'light', solverAlgorithm: 'mccfr' }),
      )
    })
    await page.goto('/simulate')

    const picker = page.getByTestId('bot-picker')
    const name = picker.getByTestId('bot-algorithm-name')

    // The stored mode hides data, so the battle reads the imperfect search.
    // The migrated `solverAlgorithm` supplies it, and the stored `botAlgorithm`
    // never reaches this line.
    await expect(name).toHaveText('MCCFR (sampled belief)')

    // A mode that holds no belief reads the other dropdown, which the migration
    // left at its default because `mccfr` is not a perfect-information search.
    await pickSelectOption(page, 'Information mode', 'Perfect Information')
    await expect(name).toHaveText('Double Oracle (exact)')

    // Both choices survive a reload, and the mode still selects between them.
    await page.reload()
    await expect(name).toHaveText('Double Oracle (exact)')
    await pickSelectOption(page, 'Information mode', 'Closed Team Sheet')
    await expect(name).toHaveText('MCCFR (sampled belief)')
  })

  test('the reveal names the solver strategy under perfect information', async ({
    page,
  }, testInfo) => {
    await seedTeam(page)
    await page.goto('/simulate')
    await pickSelectOption(page, 'Ruleset', 'Pokémon Champions Season 2 Singles')
    // An exact algorithm reads the true position, so it can only control P2
    // when the session hides nothing. The picker permits the pair only in this
    // mode.
    await pickSelectOption(page, 'Information mode', 'Perfect Information')

    // Keep this UI test small. Solver correctness and unrestricted execution
    // have Rust tests.
    await page.route('**/api/battles', async (route) => {
      const request = route.request().postDataJSON()
      request.botP2.simulationTurnBudget = 20
      await route.continue({ postData: JSON.stringify(request) })
    })

    const picker = page.getByTestId('bot-picker')
    await picker.getByRole('button', { name: 'None' }).click()
    await picker.getByRole('option', { name: 'Solver', exact: true }).click()
    // Perfect Information holds no belief, so the battle reads the
    // perfect-information search. No belief search can reach this mode at all,
    // because the two dropdowns keep the categories apart.
    await expect(picker.getByTestId('bot-algorithm-name')).toHaveText('Double Oracle (exact)')
    await expect(picker.getByTestId('bot-algorithm-hint')).toContainText('Exact')
    await expect(picker.getByTestId('bot-algorithm-limit')).toContainText(
      'Perfect Information holds no belief',
    )

    // The reveal takes no opt-in control. `reveals_strategy` in
    // `poke_rust/src/bin/server/analysis.rs` reads the profile alone, so every
    // bot session shows the rows and the picker says so.
    await expect(picker).toContainText('Its live strategy stays visible')
    await page.screenshot({
      path: testInfo.outputPath('opponent-strategy-setting.png'),
      fullPage: true,
      animations: 'disabled',
    })

    await page.getByRole('button', { name: 'Start Battle' }).click()
    const previewMons = page.getByTestId('preview-mon')
    await expect(previewMons.first()).toBeVisible({ timeout: 10_000 })
    await expect(page.getByTestId('bot-badge-reveal')).toHaveText('live strategy')

    // Expand the one solver card. The first analysis answer shows the full
    // preview strategy before P1 picks.
    await page.getByTestId('bot-badge').getByRole('button').first().click()
    const reveal = page.getByTestId('p2-reveal')
    await expect(reveal).toContainText('Player 2 strategy', { timeout: 20_000 })
    const detail = page.getByTestId('p2-reveal-detail')
    const currentStrategy = page.getByTestId('p2-strategy-current')
    await expect(currentStrategy).toContainText('Player 2 strategy now')
    await expect(currentStrategy).toContainText('Lead')
    await expect(currentStrategy).toContainText('%')
    await page.screenshot({
      path: testInfo.outputPath('opponent-strategy-preview.png'),
      fullPage: true,
      animations: 'disabled',
    })

    const count = await previewMons.count()
    for (let i = 0; i < count; i++) await previewMons.nth(i).click()
    await page.getByTestId('preview-confirm').click()

    // The resolved preview keeps its source strategy and starts the battle search.
    await expect(page.getByTestId('move-option').first()).toBeVisible({ timeout: 10_000 })
    const drawnStrategy = page.getByTestId('p2-strategy-drawn')
    await expect(drawnStrategy).toContainText('Strategy of the last draw')
    await expect(drawnStrategy).toContainText('\u25b8')
    await expect(currentStrategy).toContainText('Player 2 strategy now', { timeout: 20_000 })
    await page.screenshot({
      path: testInfo.outputPath('opponent-strategy-battle.png'),
      fullPage: true,
      animations: 'disabled',
    })

    // Hold one current-position response until the next position is on screen.
    // The delayed response uses a sentinel command to expose a stale write.
    let releaseHeldResponse!: () => void
    const heldResponse = new Promise<void>((resolve) => {
      releaseHeldResponse = resolve
    })
    let markResponseCaptured!: () => void
    const responseCaptured = new Promise<void>((resolve) => {
      markResponseCaptured = resolve
    })
    let holdNextResponse = true
    await page.route('**/api/battles/*/analysis', async (route) => {
      if (!holdNextResponse) {
        await route.continue()
        return
      }
      holdNextResponse = false
      const response = await route.fetch()
      const body = await response.json()
      const firstCommand = body.checkpoint?.p2Strategy?.rows?.[0]?.commands?.[0]
      if (firstCommand) firstCommand.description = 'STALE STRATEGY FROM THE PREVIOUS POSITION'
      markResponseCaptured()
      await heldResponse
      await route.fulfill({ response, json: body })
    })
    await responseCaptured

    // The wait line replaces the reveal while the search runs.
    await page.getByTestId('move-option').first().click()
    await expect(page.getByText('Turn 1', { exact: true })).toBeVisible({ timeout: 20_000 })
    releaseHeldResponse()
    await page.waitForTimeout(150)
    await expect(detail).not.toContainText('STALE STRATEGY FROM THE PREVIOUS POSITION')
    await expect(reveal).toContainText('from the solver strategy')
    await expect(reveal).toContainText('doubleOracle')
    await expect(reveal).toContainText('turn 1')
    await expect(drawnStrategy).toContainText('%')
    // The strategy contains action rates, but it contains no solver win odds.
    await expect(detail).not.toContainText(/win/i)
    await page.screenshot({
      path: testInfo.outputPath('opponent-strategy-drawn.png'),
      fullPage: true,
      animations: 'disabled',
    })
  })
})
