import { useBattle } from '../../store/battleStore'
import Arena from './Arena'
import BattleLogSidebar from './BattleLogSidebar'
import ControlPanel from './ControlPanel'
import TeamInfoSidebar from './TeamInfoSidebar'

export default function BattleScreen() {
  const { view, error, clearError, leave } = useBattle()
  if (!view) return null

  // GameOverState carries only the winner; teamPreview carries only the
  // preview — the arena needs live side data.
  const inBattle = !!view.p1 && !!view.p2

  return (
    <div className="flex h-[calc(100vh-3.5rem)] gap-3 p-3">
      <BattleLogSidebar />

      <div className="relative flex flex-1 flex-col">
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

      <TeamInfoSidebar />

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
