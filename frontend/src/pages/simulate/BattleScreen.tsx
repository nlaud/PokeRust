import { useState } from 'react'
import ConfirmDialog from '../../components/common/ConfirmDialog'
import { useBattle } from '../../store/battleStore'
import Arena from './Arena'
import BattleLogSidebar from './BattleLogSidebar'
import ControlPanel from './ControlPanel'
import TeamInfoSidebar from './TeamInfoSidebar'

export default function BattleScreen() {
  const { view, error, clearError, leave } = useBattle()
  const [confirmLeave, setConfirmLeave] = useState(false)
  if (!view) return null

  // Show the arena only when the view contains live side data.
  const inBattle = !!view.p1 && !!view.p2

  return (
    <div className="flex flex-col gap-3 p-3 lg:h-full lg:flex-row">
      <div className="order-2 lg:order-none lg:contents">
        <BattleLogSidebar />
      </div>

      <div className="relative order-1 flex min-h-[440px] min-w-0 flex-1 flex-col lg:order-none">
        {inBattle ? (
          <Arena />
        ) : (
          <div className="flex flex-1 items-start justify-center rounded-card bg-gradient-to-b from-sky-100 to-emerald-100 pt-10 dark:from-slate-800 dark:to-slate-700">
            {view.phase === 'teamPreview' && (
              <span className="glass rounded-card px-4 py-2 text-sm font-semibold">Team Preview</span>
            )}
          </div>
        )}

        <ControlPanel />

        {view.phase !== 'gameOver' && (
          <button
            onClick={() => setConfirmLeave(true)}
            className="lift absolute bottom-3 right-3 z-10 rounded-card border border-subtle bg-card px-3 py-1.5 text-xs font-semibold text-ink-muted shadow-sm hover:text-danger"
          >
            New battle
          </button>
        )}

        {view.phase === 'gameOver' && (
          <div className="absolute inset-0 z-30 flex items-center justify-center rounded-card bg-black/40">
            <div className="glass rounded-card p-8 text-center shadow-lg">
              <h2 className="text-2xl font-bold">
                {view.winner
                  ? `${view.winner === 'p1' ? 'Player 1' : 'Player 2'} wins!`
                  : "It's a draw!"}
              </h2>
              <button
                onClick={leave}
                className="lift mt-4 rounded-card bg-primary px-4 py-2 text-sm font-semibold text-white"
              >
                New battle
              </button>
            </div>
          </div>
        )}
      </div>

      <div className="order-3 lg:order-none lg:contents">
        <TeamInfoSidebar />
      </div>

      {confirmLeave && (
        <ConfirmDialog
          title="End this battle?"
          message="The current battle will be abandoned and you'll return to the new-battle setup."
          onConfirm={() => {
            setConfirmLeave(false)
            leave()
          }}
          onCancel={() => setConfirmLeave(false)}
        />
      )}

      {error && (
        <div className="fixed bottom-4 left-1/2 z-50 flex -translate-x-1/2 items-center gap-3 rounded-card bg-danger px-4 py-2 text-sm font-medium text-white shadow-lg">
          <span>{error}</span>
          <button onClick={clearError} className="font-bold" aria-label="Dismiss error">
            ✕
          </button>
        </div>
      )}
    </div>
  )
}
