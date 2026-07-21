import type { FieldSlot, PlayerId } from '../../api/types'
import Sprite from '../../components/common/Sprite'
import { useBattle } from '../../store/battleStore'
import FieldIndicators from './FieldIndicators'
import PokemonHUD from './PokemonHUD'

function slotEquals(a: FieldSlot, b: FieldSlot) {
  return a.player === b.player && a.slotIndex === b.slotIndex
}

function otherPlayer(player: PlayerId): PlayerId {
  return player === 'p1' ? 'p2' : 'p1'
}

export default function Arena() {
  const { view, currentPlayer, pendingAttack, pushSlotCommand } = useBattle()
  if (!view?.p1 || !view.p2) return null

  // Whose side of the field is "the viewer" — bottom, facing away, HUD above —
  // vs "the opponent" — top, facing the viewer, HUD below. Follows whoever is
  // currently choosing moves: the battlefield flips when the hotseat wizard
  // advances from P1's command entry to P2's (see `activeView` in
  // battleStore.ts, which the `view` this component reads already tracks).
  const viewer = currentPlayer
  const opponent = otherPlayer(viewer)

  // Doubles target-pick mode: legal target slots come straight from the
  // server's pre-expanded command options.
  const targetableSlots: FieldSlot[] =
    pendingAttack?.targets
      .map((t) => (t.command.kind === 'attack' || t.command.kind === 'struggle' ? t.command.target : undefined))
      .filter((t): t is FieldSlot => t !== undefined) ?? []

  const pickTarget = (slot: FieldSlot) => {
    const option = pendingAttack?.targets.find((t) => {
      const target =
        t.command.kind === 'attack' || t.command.kind === 'struggle' ? t.command.target : undefined
      return target && slotEquals(target, slot)
    })
    if (option) pushSlotCommand(option.command)
  }

  // Each mon column scales with the horizontal room (flex-1 inside a
  // percentage-width side container) up to a hard cap, so sprites and HP bars
  // actually grow on wide windows instead of just drifting to the edges. The
  // viewer's own HUD sits ABOVE its sprite — closer to the middle of the arena.
  const renderSide = (player: PlayerId) => {
    const isOwnSide = player === viewer
    const side = player === 'p1' ? view.p1! : view.p2!
    return side.active.map((mon, slotIndex) => {
      const slot: FieldSlot = { player, slotIndex }
      const targetable = targetableSlots.some((t) => slotEquals(t, slot))
      const sprite = (
        <div
          className={`w-full rounded-card ${targetable ? 'animate-pulse ring-4 ring-danger' : ''}`}
        >
          <Sprite
            species={mon.species}
            facing={isOwnSide ? 'back' : 'front'}
            size={isOwnSide ? 192 : 160}
            className={`h-auto w-full ${mon.fainted ? 'opacity-30 grayscale' : ''}`}
          />
        </div>
      )
      return (
        <div
          key={`${player}-${slotIndex}`}
          className={`flex min-w-0 flex-1 flex-col items-center gap-1 ${
            isOwnSide ? 'max-w-80' : 'max-w-72'
          } ${targetable ? 'cursor-pointer' : ''}`}
          onClick={targetable ? () => pickTarget(slot) : undefined}
        >
          {/* Below xl the HUD overlaps the sprite (negative margin + z-index)
              so the column fits smaller arenas without pushing anything out. */}
          {isOwnSide && <div className="z-10 w-full max-xl:-mb-12"><PokemonHUD mon={mon} /></div>}
          {sprite}
          {!isOwnSide && <div className="z-10 w-full max-xl:-mt-12"><PokemonHUD mon={mon} /></div>}
        </div>
      )
    })
  }

  return (
    <div className="relative flex-1 overflow-hidden rounded-card bg-gradient-to-b from-sky-100 to-emerald-100 dark:from-slate-800 dark:to-slate-700">
      <FieldIndicators view={view} />

      {pendingAttack && (
        <div className="absolute inset-x-0 top-3 z-10 text-center">
          <span className="glass rounded-card px-3 py-1 text-xs font-semibold">
            Choose a target
          </span>
        </div>
      )}

      {/* Opponent top-right, viewer bottom-left — flips with `viewer` when the
          hotseat wizard switches whose moves are being chosen. Side containers
          are capped in width so the two teams' HP bars can never overlap. */}
      <div className="absolute right-6 top-6 flex w-[45%] max-w-[38rem] justify-end gap-4">
        {renderSide(opponent)}
      </div>
      <div className="absolute bottom-40 left-6 flex w-[48%] max-w-[42rem] gap-4">
        {renderSide(viewer)}
      </div>
    </div>
  )
}
