import type { FieldSlot, PlayerId } from '../../api/types'
import Sprite from '../../components/common/Sprite'
import { useTracker } from '../../store/trackerStore'
import FieldIndicators from '../simulate/FieldIndicators'
import PokemonHUD from '../simulate/PokemonHUD'

// Uses the simulator arena layout with tracker state.
// Tracker mode has one fixed player perspective.
// It does not need hotseat or target-selection state.

const VIEWER: PlayerId = 'p1'
const OPPONENT: PlayerId = 'p2'

export default function TrackerArena() {
  // Show the uncommitted structural preview while the user types.
  // Otherwise, show the last committed view.
  const view = useTracker((s) => s.previewView ?? s.view)
  if (!view?.p1 || !view.p2) return null

  // Scale each Pokémon column to the available width and a fixed maximum.
  // Put the player's HUD above its sprite.
  const renderSide = (player: PlayerId) => {
    const isOwnSide = player === VIEWER
    const side = player === 'p1' ? view.p1! : view.p2!
    return side.active.map((mon, slotIndex) => {
      const slot: FieldSlot = { player, slotIndex }
      return (
        <div
          key={`${slot.player}-${slot.slotIndex}`}
          className={`flex min-w-0 flex-1 flex-col items-center gap-1 ${
            isOwnSide ? 'max-w-80' : 'max-w-72'
          }`}
        >
          {/* Below xl the HUD overlaps the sprite (negative margin + z-index)
              so the column fits smaller arenas without pushing anything out. */}
          {isOwnSide && <div className="z-10 w-full max-xl:-mb-12"><PokemonHUD mon={mon} /></div>}
          <div className="w-full rounded-card">
            <Sprite
              species={mon.species}
              facing={isOwnSide ? 'back' : 'front'}
              size={isOwnSide ? 192 : 160}
              className={`h-auto w-full ${mon.fainted ? 'opacity-30 grayscale' : ''}`}
            />
          </div>
          {!isOwnSide && <div className="z-10 w-full max-xl:-mt-12"><PokemonHUD mon={mon} /></div>}
        </div>
      )
    })
  }

  return (
    <div className="relative flex-1 overflow-hidden rounded-card bg-gradient-to-b from-sky-100 to-emerald-100 dark:from-slate-800 dark:to-slate-700">
      <FieldIndicators view={view} />

      {/* Opponent top-right, viewer bottom-left — same fixed layout as battle
          mode's Arena, just never flips (tracker has one perspective only). */}
      <div className="absolute right-6 top-6 flex w-[45%] max-w-[38rem] justify-end gap-4">
        {renderSide(OPPONENT)}
      </div>
      <div className="absolute bottom-40 left-6 flex w-[48%] max-w-[42rem] gap-4">
        {renderSide(VIEWER)}
      </div>
    </div>
  )
}
