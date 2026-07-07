import { create } from 'zustand'
import * as api from '../api/client'
import type {
  BattleCommand,
  BattleView,
  EventNode,
  LegalCommands,
  PlayerCommand,
  PlayerId,
  TurnLogEntry,
} from '../api/types'

/**
 * Hotseat command wizard. One user enters P1's commands slot by slot, then
 * P2's, and the pair is submitted as a single turn request. `draftCommands`
 * accumulates the current player's per-slot picks; a slot awaiting a doubles
 * target keeps its partial attack in `pendingAttack`.
 */
export interface PendingAttack {
  moveSlot: number
  terastallize: boolean
  megaEvolve: boolean
  /** Target options for this move, keyed from the server's pre-expanded list. */
  targets: { command: BattleCommand; description: string }[]
}

interface BattleStore {
  battleId: string | null
  view: BattleView | null
  log: TurnLogEntry[]
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

export const useBattle = create<BattleStore>((set, get) => {
  /** After both players' commands exist, ship the turn to the server. */
  async function maybeSubmitTurn() {
    const { battleId, view, p1Commands, draftCommands, currentPlayer, commands, busy } = get()
    if (!battleId || !view || !commands) return
    // Re-entrancy guard: the auto-fill path and the ControlPanel's forced-slot
    // effect can both complete a draft; without this, the same turn gets
    // POSTed twice and the second submit re-validates stale commands against
    // the already-advanced battle ("… is not a legal command", at random).
    if (busy) return

    const slotsNeeded = commands.slots.length
    if (draftCommands.length < slotsNeeded) return

    const playerCommand: PlayerCommand = { kind: 'battle', commands: draftCommands }

    if (currentPlayer === 'p1') {
      // P1 done — flip to P2 and fetch their legal commands.
      set({ p1Commands: playerCommand, currentPlayer: 'p2', draftCommands: [], pendingAttack: null, commands: null })
      await get().fetchCommands()
      await autoFillForcedSlots()
      return
    }

    // P2 done — submit the full turn.
    set({ busy: true, error: null })
    try {
      const turnLabel = view.phase === 'teamPreview' ? 'Team Preview' : `Turn ${view.turnNumber}`
      const response = await api.submitTurn(battleId, {
        p1: p1Commands ?? { kind: 'pass' },
        p2: playerCommand,
      })
      set((s) => ({
        view: response.state,
        probability: response.probability,
        log: appendLog(s.log, turnLabel, response.events),
        currentPlayer: 'p1',
        draftCommands: [],
        p1Commands: null,
        pendingAttack: null,
        commands: null,
        busy: false,
      }))
      if (response.state.phase !== 'gameOver') {
        await get().fetchCommands()
        await autoFillForcedSlots()
      }
    } catch (err) {
      // Reset the offending player's draft; keep P1's committed commands if P2 failed.
      set({
        error: err instanceof Error ? err.message : String(err),
        draftCommands: [],
        pendingAttack: null,
        busy: false,
      })
    }
  }

  /**
   * In selfSwitch/replacement phases most slots (or a whole player) are forced
   * to Pass. Auto-fill forced slots so the user only ever clicks real choices;
   * if every slot of the current player is forced, submit for them silently.
   */
  async function autoFillForcedSlots() {
    const { commands, busy, draftCommands } = get()
    if (!commands) return
    if (commands.phase !== 'selfSwitch' && commands.phase !== 'replacement') return
    // If a submit is in flight, or the ControlPanel's forced-slot effect has
    // already begun filling this draft, leave the draft alone — overwriting it
    // here can shrink it mid-flow or double-submit the turn.
    if (busy || draftCommands.length > 0) return

    const allForced = commands.slots.every((slot) => slot.forced)
    if (allForced) {
      set({ draftCommands: commands.slots.map(() => ({ kind: 'pass' }) as BattleCommand) })
      await maybeSubmitTurn()
      return
    }
    // Leading forced slots are auto-filled; the wizard stops at the first real choice.
    const draft: BattleCommand[] = []
    for (const slot of commands.slots) {
      if (slot.forced) draft.push(slot.options[0].command)
      else break
    }
    set({ draftCommands: draft })
  }

  return {
    battleId: null,
    view: null,
    log: [],
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
        set({
          battleId: response.battleId,
          view: response.state,
          log: [],
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
        set({
          battleId,
          view: response.state,
          log: response.log,
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
        view: null,
        log: [],
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
      // Back rewinds within the current player's action first (pending target,
      // then the last committed slot); at the very beginning of P2's turn it
      // rewinds to the beginning of P1's turn.
      const { pendingAttack, draftCommands, view, currentPlayer, commands, previewPicks } = get()
      if (view?.phase === 'teamPreview') {
        if (previewPicks.length > 0) {
          set({ previewPicks: [] })
          return
        }
        if (currentPlayer === 'p2') {
          set({ currentPlayer: 'p1', p1Commands: null, previewPicks: [] })
        }
        return
      }
      if (pendingAttack) {
        set({ pendingAttack: null })
        return
      }
      // Unwind forced auto-fills sitting on top of the last real choice, then
      // drop that choice. The forced-slot effect re-fills forward as needed.
      const slots = commands?.slots ?? []
      let end = draftCommands.length
      while (end > 0 && slots[end - 1]?.forced) end -= 1
      if (end > 0) {
        set({ draftCommands: draftCommands.slice(0, end - 1) })
        return
      }
      // Nothing left to unwind for this player — flip back to P1's turn.
      if (currentPlayer === 'p2') {
        set({
          currentPlayer: 'p1',
          p1Commands: null,
          draftCommands: [],
          pendingAttack: null,
          commands: null,
        })
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
        set({ p1Commands: command, currentPlayer: 'p2', previewPicks: [] })
        return
      }

      set({ busy: true, error: null })
      try {
        const response = await api.submitTurn(battleId, {
          p1: p1Commands ?? { kind: 'pass' },
          p2: command,
        })
        set((s) => ({
          view: response.state,
          probability: response.probability,
          log: appendLog(s.log, 'Team Preview', response.events),
          currentPlayer: 'p1',
          previewPicks: [],
          p1Commands: null,
          commands: null,
          busy: false,
        }))
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
