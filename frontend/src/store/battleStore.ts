import { create } from 'zustand'
import * as api from '../api/client'
import type {
  BattleCommand,
  BattleView,
  BotProfileView,
  EventNode,
  LegalCommands,
  P2Reveal,
  PlayerCommand,
  PlayerId,
  TurnLogEntry,
} from '../api/types'
import { megaFormeNames, preloadSprites } from '../lib/sprites'

/** Loads sprites for each battle Pokémon and possible Mega form. */
function preloadBattleSprites(view: BattleView) {
  const mons = [
    ...(view.preview ? [...view.preview.p1Mons, ...view.preview.p2Mons] : []),
    ...(view.p1 ? [...view.p1.active, ...view.p1.back] : []),
    ...(view.p2 ? [...view.p2.active, ...view.p2.back] : []),
  ]
  preloadSprites(mons.flatMap((m) => [m.species, ...megaFormeNames(m.species, m.item)]))
}

/** Stores commands for hotseat battles and P2 bot battles. */
export interface PendingAttack {
  moveSlot: number
  terastallize: boolean
  megaEvolve: boolean
  /** Server-provided target options for this move. */
  targets: { command: BattleCommand; description: string }[]
}

/** Selects the view or log for the current hotseat player. */
function pickForPlayer<T>(p1Value: T, p2Value: T, currentPlayer: PlayerId): T {
  return currentPlayer === 'p1' ? p1Value : p2Value
}

interface BattleStore {
  battleId: string | null
  /** P1's fog-of-war view of the battle (cached from the last response). */
  viewP1: BattleView | null
  /** P2's fog-of-war view of the same battle (cached from the last response). */
  viewP2: BattleView | null
  /** View for `currentPlayer`. */
  view: BattleView | null
  /** Turn events masked for P1. */
  logP1: TurnLogEntry[]
  /** Turn events masked for P2. */
  logP2: TurnLogEntry[]
  /** Log for `currentPlayer`. */
  log: TurnLogEntry[]
  /** The resolved profile for the P2 bot.
   * `null` means that the battle has no profile. */
  botP2: BotProfileView | null
  /** The command that the server drew for P2 on the last resolved turn.
   * `null` in a hotseat battle, and at team preview. */
  p2Reveal: P2Reveal | null
  /** True while the client waits for the analysis job of the P2 bot. */
  waitingForBot: boolean
  /** How long the P2 search has run, while the client waits for it. */
  botWaitMs: number | null
  /** True after Player 1 asks to stop the wait and change the move. */
  botWaitCancelled: boolean
  /** True after one search ended with no answer for this position.
   * The next submission then plays the turn without a wait. */
  botNoAnswer: boolean
  probability: number | null
  error: string | null
  busy: boolean

  currentPlayer: PlayerId
  commands: LegalCommands | null
  draftCommands: BattleCommand[]
  p1Commands: PlayerCommand | null
  pendingAttack: PendingAttack | null

  /** Team preview picks (indices into the current player's preview mons). */
  previewPicks: number[]

  createBattle: (req: Parameters<typeof api.createBattle>[0]) => Promise<void>
  restore: (battleId: string) => Promise<void>
  leave: () => void
  fetchCommands: () => Promise<void>
  pushSlotCommand: (command: BattleCommand) => void
  setPendingAttack: (pending: PendingAttack | null) => void
  goBack: () => void
  /** Stops the wait for the P2 search and returns the move choice. */
  cancelBotWait: () => void
  togglePreviewPick: (index: number) => void
  submitPreview: () => Promise<void>
  clearError: () => void
  showError: (message: string) => void
}

const BATTLE_ID_KEY = 'pokerust.activeBattleId'
const ANALYSIS_POLL_MS = 100

/** What one wait for the Player 2 search produced. */
type BotWaitResult = 'answered' | 'noAnswer' | 'cancelled'

/** The message that a search with no answer shows to Player 1.
 *
 * The team preview and a battle turn both show it, so the line names no
 * command type. */
const BOT_NO_ANSWER =
  'The search produced no answer for this position. Submit again to play it with a random Player 2 choice, or change your selection.'

/**
 * Waits for the search that supplies Player 2's strategy.
 *
 * The wait ends when the job stops, and no fixed limit ends it early. A search
 * that runs past its own time limit therefore still supplies the strategy. A
 * turn simulation that already runs cannot stop, which is why that overrun
 * happens.
 *
 * `shouldStop` returns true when Player 1 cancels the wait.
 */
async function waitForBotAnalysis(
  battleId: string,
  shouldStop: () => boolean,
  onProgress: (elapsedMs: number | null) => void,
): Promise<BotWaitResult> {
  for (;;) {
    if (shouldStop()) return 'cancelled'
    const progress = await api.getAnalysis(battleId)
    const current =
      progress.checkpoint !== null &&
      progress.checkpoint.generation === progress.generation &&
      !progress.checkpoint.stale
    if (progress.phase !== 'running') {
      if (!current) {
        // The server console holds the reason. This line holds the position
        // that asked for it, so the two logs line up.
        console.warn('bot search: no answer for this position', {
          phase: progress.phase,
          generation: progress.generation,
          checkpointGeneration: progress.checkpoint?.generation ?? null,
          checkpointStale: progress.checkpoint?.stale ?? null,
          error: progress.error,
        })
      }
      return current ? 'answered' : 'noAnswer'
    }
    onProgress(progress.runningMs)
    await new Promise((resolve) => window.setTimeout(resolve, ANALYSIS_POLL_MS))
  }
}

function appendLog(log: TurnLogEntry[], label: string, events: EventNode[]): TurnLogEntry[] {
  return [...log, { label, events }]
}

/** Adds one turn to both masked logs. */
function appendDualLog(
  logP1: TurnLogEntry[],
  logP2: TurnLogEntry[],
  label: string,
  eventsP1: EventNode[],
  eventsP2: EventNode[],
): { logP1: TurnLogEntry[]; logP2: TurnLogEntry[] } {
  return {
    logP1: appendLog(logP1, label, eventsP1),
    logP2: appendLog(logP2, label, eventsP2),
  }
}

export const useBattle = create<BattleStore>((set, get) => {
  /** Submits one battle command pair and stores the resolved position. */
  async function submitBattleTurn(
    battleId: string,
    p1: PlayerCommand,
    p2: PlayerCommand | undefined,
    turnLabel: string,
  ) {
    try {
      const response = await api.submitTurn(battleId, { p1, ...(p2 ? { p2 } : {}) })
      set((s) => {
        const { logP1, logP2 } = appendDualLog(
          s.logP1,
          s.logP2,
          turnLabel,
          response.events,
          response.eventsP2,
        )
        return {
          viewP1: response.state,
          viewP2: response.stateP2,
          view: pickForPlayer(response.state, response.stateP2, 'p1'),
          probability: response.probability,
          logP1,
          logP2,
          log: pickForPlayer(logP1, logP2, 'p1'),
          currentPlayer: 'p1' as const,
          draftCommands: [],
          p1Commands: null,
          pendingAttack: null,
          commands: null,
          busy: false,
          // A hotseat turn returns no reveal, so this also clears the last one.
          p2Reveal: response.p2Reveal ?? null,
        }
      })
      if (response.state.phase !== 'gameOver') {
        await get().fetchCommands()
        await autoFillForcedSlots()
      }
    } catch (err) {
      set({
        error: err instanceof Error ? err.message : String(err),
        draftCommands: [],
        pendingAttack: null,
        busy: false,
      })
    }
  }

  /** After both players' commands exist, ship the turn to the server. */
  async function maybeSubmitTurn() {
    const { battleId, view, p1Commands, draftCommands, currentPlayer, commands, busy } = get()
    if (!battleId || !view || !commands) return
    // Prevent two paths from submitting the same completed draft.
    // A second request would validate stale commands against the next state.
    if (busy) return

    const slotsNeeded = commands.slots.length
    if (draftCommands.length < slotsNeeded) return

    const playerCommand: PlayerCommand = { kind: 'battle', commands: draftCommands }

    if (currentPlayer === 'p1') {
      const bot = get().botP2
      if (bot) {
        // The last search ended with no answer, so this submission plays the
        // turn. The server then draws one legal command for Player 2. Without
        // this path the position could never resolve.
        if (get().botNoAnswer) {
          set({ busy: true, error: null, botNoAnswer: false, p2Reveal: null })
          await submitBattleTurn(battleId, playerCommand, undefined, `Turn ${view.turnNumber}`)
          return
        }
        // P2 has no hotseat step, so the client waits for the search that
        // supplies P2's strategy. The wait ends when the job stops.
        set({
          busy: true,
          error: null,
          waitingForBot: true,
          botWaitCancelled: false,
          botWaitMs: null,
          p2Reveal: null,
        })
        const outcome = await waitForBotAnalysis(
          battleId,
          () => get().botWaitCancelled,
          (elapsed) => set({ botWaitMs: elapsed }),
        ).catch((): BotWaitResult => 'noAnswer')
        set({ waitingForBot: false, botWaitMs: null })
        if (outcome === 'cancelled') {
          // Player 1 asked for the move choice again. The search keeps running,
          // so the next submission can still read its answer.
          set({
            busy: false,
            botWaitCancelled: false,
            draftCommands: [],
            pendingAttack: null,
          })
          return
        }
        if (outcome === 'noAnswer') {
          set({
            busy: false,
            botNoAnswer: true,
            error: BOT_NO_ANSWER,
            draftCommands: [],
            pendingAttack: null,
          })
          return
        }
        await submitBattleTurn(battleId, playerCommand, undefined, `Turn ${view.turnNumber}`)
        return
      }
      // Show P2's view and get P2's legal commands.
      set((s) => ({
        p1Commands: playerCommand,
        currentPlayer: 'p2',
        view: pickForPlayer(s.viewP1, s.viewP2, 'p2'),
        log: pickForPlayer(s.logP1, s.logP2, 'p2'),
        draftCommands: [],
        pendingAttack: null,
        commands: null,
      }))
      await get().fetchCommands()
      await autoFillForcedSlots()
      return
    }

    // Submit the complete turn after P2 selects commands.
    set({ busy: true, error: null })
    await submitBattleTurn(
      battleId,
      p1Commands ?? { kind: 'pass' },
      playerCommand,
      `Turn ${view.turnNumber}`,
    )
  }

  /**
   * Fills forced commands during self-switch and replacement phases.
   * Submits automatically when the current player has no choice.
   */
  async function autoFillForcedSlots() {
    const { commands, busy, draftCommands } = get()
    if (!commands) return
    if (commands.phase !== 'selfSwitch' && commands.phase !== 'replacement') return
    // Do not change a draft during submission or forced-slot completion.
    // A change can remove commands or submit the turn twice.
    if (busy || draftCommands.length > 0) return

    const allForced = commands.slots.every((slot) => slot.forced)
    if (allForced) {
      set({ draftCommands: commands.slots.map(() => ({ kind: 'pass' }) as BattleCommand) })
      await maybeSubmitTurn()
      return
    }
    // Fill initial forced slots and stop at the first choice.
    const draft: BattleCommand[] = []
    for (const slot of commands.slots) {
      if (slot.forced) draft.push(slot.options[0].command)
      else break
    }
    set({ draftCommands: draft })
  }

  return {
    battleId: null,
    viewP1: null,
    viewP2: null,
    view: null,
    logP1: [],
    logP2: [],
    log: [],
    botP2: null,
    p2Reveal: null,
    waitingForBot: false,
    botWaitMs: null,
    botWaitCancelled: false,
    botNoAnswer: false,
    probability: null,
    error: null,
    busy: false,
    currentPlayer: 'p1',
    commands: null,
    draftCommands: [],
    p1Commands: null,
    pendingAttack: null,
    previewPicks: [],

    createBattle: async (req) => {
      set({ busy: true, error: null })
      try {
        const response = await api.createBattle(req)
        sessionStorage.setItem(BATTLE_ID_KEY, response.battleId)
        preloadBattleSprites(response.state)
        set({
          battleId: response.battleId,
          viewP1: response.state,
          viewP2: response.stateP2,
          view: pickForPlayer(response.state, response.stateP2, 'p1'),
          logP1: [],
          logP2: [],
          log: [],
          botP2: response.botP2,
          p2Reveal: null,
          waitingForBot: false,
          botWaitMs: null,
          botWaitCancelled: false,
          botNoAnswer: false,
          probability: null,
          currentPlayer: 'p1',
          draftCommands: [],
          p1Commands: null,
          pendingAttack: null,
          previewPicks: [],
          commands: null,
          busy: false,
        })
      } catch (err) {
        set({ error: err instanceof Error ? err.message : String(err), busy: false })
      }
    },

    restore: async (battleId) => {
      try {
        const response = await api.getBattle(battleId)
        preloadBattleSprites(response.state)
        set({
          battleId,
          viewP1: response.state,
          viewP2: response.stateP2,
          view: pickForPlayer(response.state, response.stateP2, 'p1'),
          logP1: response.log,
          logP2: response.logP2,
          log: pickForPlayer(response.log, response.logP2, 'p1'),
          botP2: response.botP2,
          // A restore reads the session, and the session keeps no past reveal.
          p2Reveal: null,
          waitingForBot: false,
          botWaitMs: null,
          botWaitCancelled: false,
          botNoAnswer: false,
          currentPlayer: 'p1',
          draftCommands: [],
          p1Commands: null,
          pendingAttack: null,
          previewPicks: [],
          commands: null,
        })
        if (response.state.phase !== 'gameOver' && response.state.phase !== 'teamPreview') {
          await get().fetchCommands()
          await autoFillForcedSlots()
        }
      } catch {
        sessionStorage.removeItem(BATTLE_ID_KEY)
      }
    },

    leave: () => {
      sessionStorage.removeItem(BATTLE_ID_KEY)
      set({
        battleId: null,
        viewP1: null,
        viewP2: null,
        view: null,
        logP1: [],
        logP2: [],
        log: [],
        botP2: null,
        p2Reveal: null,
        waitingForBot: false,
        botWaitMs: null,
        botWaitCancelled: false,
        botNoAnswer: false,
        probability: null,
        error: null,
        currentPlayer: 'p1',
        commands: null,
        draftCommands: [],
        p1Commands: null,
        pendingAttack: null,
        previewPicks: [],
      })
    },

    fetchCommands: async () => {
      const { battleId, currentPlayer } = get()
      if (!battleId) return
      const commands = await api.getCommands(battleId, currentPlayer)
      set({ commands })
    },

    pushSlotCommand: (command) => {
      set((s) => ({ draftCommands: [...s.draftCommands, command], pendingAttack: null }))
      void maybeSubmitTurn()
    },

    setPendingAttack: (pendingAttack) => set({ pendingAttack }),

    goBack: () => {
      // First, go to the previous choice for the current player.
      // From the start of P2 input, go to the start of P1 input.
      const { pendingAttack, draftCommands, view, currentPlayer, commands, previewPicks } = get()
      if (view?.phase === 'teamPreview') {
        if (previewPicks.length > 0) {
          set({ previewPicks: [] })
          return
        }
        if (currentPlayer === 'p2') {
          set((s) => ({
            currentPlayer: 'p1',
            p1Commands: null,
            previewPicks: [],
            view: pickForPlayer(s.viewP1, s.viewP2, 'p1'),
            log: pickForPlayer(s.logP1, s.logP2, 'p1'),
          }))
        }
        return
      }
      if (pendingAttack) {
        set({ pendingAttack: null })
        return
      }
      // Remove forced entries after the last player choice.
      // Then remove that choice.
      const slots = commands?.slots ?? []
      let end = draftCommands.length
      while (end > 0 && slots[end - 1]?.forced) end -= 1
      if (end > 0) {
        set({ draftCommands: draftCommands.slice(0, end - 1) })
        return
      }
      // Return to P1 when P2 has no earlier choice.
      if (currentPlayer === 'p2') {
        set((s) => ({
          currentPlayer: 'p1',
          p1Commands: null,
          draftCommands: [],
          pendingAttack: null,
          commands: null,
          view: pickForPlayer(s.viewP1, s.viewP2, 'p1'),
          log: pickForPlayer(s.logP1, s.logP2, 'p1'),
        }))
        void get().fetchCommands()
      }
    },

    cancelBotWait: () => {
      // The wait loop reads this flag between two reads of the job. The search
      // itself keeps running on the server, so the next submission can read its
      // answer.
      if (get().waitingForBot) set({ botWaitCancelled: true })
    },

    togglePreviewPick: (index) => {
      set((s) => ({
        previewPicks: s.previewPicks.includes(index)
          ? s.previewPicks.filter((i) => i !== index)
          : [...s.previewPicks, index],
      }))
    },

    submitPreview: async () => {
      const { view, previewPicks, currentPlayer, battleId, p1Commands } = get()
      if (!view?.preview || !battleId) return
      const active = previewPicks.slice(0, view.preview.activePerSide)
      const back = previewPicks.slice(view.preview.activePerSide)
      const command: PlayerCommand = {
        kind: 'teamPreview',
        activeIndices: active,
        backIndices: back,
      }

      if (currentPlayer === 'p1' && !get().botP2) {
        set((s) => ({
          p1Commands: command,
          currentPlayer: 'p2',
          previewPicks: [],
          view: pickForPlayer(s.viewP1, s.viewP2, 'p2'),
          log: pickForPlayer(s.logP1, s.logP2, 'p2'),
        }))
        return
      }

      // Player 2 has no hotseat step at team preview either, so the client
      // waits for the search that supplies Player 2's leads. The exits match
      // the battle turn: a cancelled wait returns the picks to Player 1, and a
      // search with no answer lets the next submission play the preview with a
      // uniform draw. The picks stay in place, so that submission is one click.
      if (get().botP2 && !get().botNoAnswer) {
        set({
          busy: true,
          error: null,
          waitingForBot: true,
          botWaitCancelled: false,
          botWaitMs: null,
          p2Reveal: null,
        })
        const outcome = await waitForBotAnalysis(
          battleId,
          () => get().botWaitCancelled,
          (elapsed) => set({ botWaitMs: elapsed }),
        ).catch((): BotWaitResult => 'noAnswer')
        set({ waitingForBot: false, botWaitMs: null })
        if (outcome === 'cancelled') {
          set({ busy: false, botWaitCancelled: false })
          return
        }
        if (outcome === 'noAnswer') {
          set({ busy: false, botNoAnswer: true, error: BOT_NO_ANSWER })
          return
        }
      }

      set({ busy: true, error: null, botNoAnswer: false })
      try {
        const response = await api.submitTurn(battleId, {
          p1: currentPlayer === 'p1' ? command : (p1Commands ?? { kind: 'pass' }),
          ...(get().botP2 ? {} : { p2: command }),
        })
        set((s) => {
          const { logP1, logP2 } = appendDualLog(s.logP1, s.logP2, 'Team Preview', response.events, response.eventsP2)
          return {
            viewP1: response.state,
            viewP2: response.stateP2,
            view: pickForPlayer(response.state, response.stateP2, 'p1'),
            probability: response.probability,
            logP1,
            logP2,
            log: pickForPlayer(logP1, logP2, 'p1'),
            currentPlayer: 'p1',
            previewPicks: [],
            p1Commands: null,
            commands: null,
            busy: false,
            // The preview reveal carries no command. The leads appear on the
            // field on their own, and the back picks stay under the fog of war.
            p2Reveal: null,
          }
        })
        await get().fetchCommands()
      } catch (err) {
        set({
          error: err instanceof Error ? err.message : String(err),
          previewPicks: [],
          busy: false,
        })
      }
    },

    clearError: () => set({ error: null }),

    showError: (message) => set({ error: message }),
  }
})

export function storedBattleId(): string | null {
  return sessionStorage.getItem(BATTLE_ID_KEY)
}
