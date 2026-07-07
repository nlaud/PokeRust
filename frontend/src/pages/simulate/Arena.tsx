import type { FieldSlot } from '../../api/types'
import Sprite from '../../components/common/Sprite'
import { useBattle } from '../../store/battleStore'
import FieldIndicators from './FieldIndicators'
import PokemonHUD from './PokemonHUD'

function slotEquals(a: FieldSlot, b: FieldSlot) {
  return a.player === b.player && a.slotIndex === b.slotIndex
}

export default function Arena() {
  const { view, pendingAttack, pushSlotCommand } = useBattle()
  if (!view?.p1 || !view.p2) return null

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

  const renderSide = (player: 'p1' | 'p2') => {
    const side = player === 'p1' ? view.p1! : view.p2!
    return side.active.map((mon, slotIndex) => {
      const slot: FieldSlot = { player, slotIndex }
      const targetable = targetableSlots.some((t) => slotEquals(t, slot))
      return (
        <div
          key={`${player}-${slotIndex}`}
          className={`flex flex-col items-center gap-1 ${targetable ? 'cursor-pointer' : ''}`}
          onClick={targetable ? () => pickTarget(slot) : undefined}
        >
          <div className={`rounded-card ${targetable ? 'animate-pulse ring-4 ring-danger' : ''}`}>
            <Sprite
              species={mon.species}
              facing={player === 'p2' ? 'front' : 'back'}
              size={player === 'p2' ? 96 : 112}
              className={mon.fainted ? 'opacity-30 grayscale' : ''}
            />
          </div>
          <PokemonHUD mon={mon} />
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

      {/* Opponent (P2) top-right, player (P1) bottom-left. */}
      <div className="absolute right-8 top-10 flex gap-6">{renderSide('p2')}</div>
      <div className="absolute bottom-36 left-8 flex gap-6">{renderSide('p1')}</div>
    </div>
  )
}
