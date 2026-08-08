import { create } from 'zustand'
import * as api from '../api/client'
import type {
  BattleCommand,
  BattleView,
  BotProfileView,
  EventNode,
  LegalCommands,
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

/**
 * Stores commands for the hotseat turn process.
 * P1 selects commands before P2.
 * The store submits both command sets in one turn request.
 */
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
  /** The resolved profile for the planned P2 bot.
   * `null` means that the battle has no profile. */
  botP2: BotProfileView | null
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
  togglePreviewPick: (index: number) => void
  submitPreview: () => Promise<void>
  clearError: () => void
  showError: (message: string) => void
}

const BATTLE_ID_KEY = 'pokerust.activeBattleId'

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
    try {
      const turnLabel = view.phase === 'teamPreview' ? 'Team Preview' : `Turn ${view.turnNumber}`
      // A session with a P2 bot lets the server draw P2's command, so the
      // request carries no `p2` field.
      const response = await api.submitTurn(battleId, {
        p1: p1Commands ?? { kind: 'pass' },
        ...(get().botP2 ? {} : { p2: playerCommand }),
      })
      set((s) => {
        const { logP1, logP2 } = appendDualLog(s.logP1, s.logP2, turnLabel, response.events, response.eventsP2)
        return {
          viewP1: response.state,
          viewP2: response.stateP2,
          view: pickForPlayer(response.state, response.stateP2, 'p1'),
          probability: response.probability,
          logP1,
          logP2,
          log: pickForPlayer(logP1, logP2, 'p1'),
          currentPlayer: 'p1',
          draftCommands: [],
          p1Commands: null,
          pendingAttack: null,
          commands: null,
          busy: false,
        }
      })
      if (response.state.phase !== 'gameOver') {
        await get().fetchCommands()
        await autoFillForcedSlots()
      }
    } catch (err) {
      // Reset the invalid player draft.
      // Keep P1's commands when P2 input fails.
      set({
        error: err instanceof Error ? err.message : String(err),
        draftCommands: [],
        pendingAttack: null,
        busy: false,
      })
    }
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

      if (currentPlayer === 'p1') {
        set((s) => ({
          p1Commands: command,
          currentPlayer: 'p2',
          previewPicks: [],
          view: pickForPlayer(s.viewP1, s.viewP2, 'p2'),
          log: pickForPlayer(s.logP1, s.logP2, 'p2'),
        }))
        return
      }

      set({ busy: true, error: null })
      try {
        // A session with a P2 bot lets the server draw P2's picks, so the
        // request carries no `p2` field.
        const response = await api.submitTurn(battleId, {
          p1: p1Commands ?? { kind: 'pass' },
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
