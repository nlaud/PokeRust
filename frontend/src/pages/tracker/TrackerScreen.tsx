import { useState } from 'react'
import ConfirmDialog from '../../components/common/ConfirmDialog'
import { useTracker } from '../../store/trackerStore'
import TrackerArena from './TrackerArena'
import TrackerInputBar from './TrackerInputBar'
import TrackerLogSidebar from './TrackerLogSidebar'
import TrackerTeamSidebar from './TrackerTeamSidebar'

/**
 * Shows the tracker arena, teams, event log, and text input.
 * The arena and teams use tracker state with simulator components.
 * The input shows submission errors next to the invalid text.
 */
export default function TrackerScreen() {
  const { view, leave } = useTracker()
  const [confirmLeave, setConfirmLeave] = useState(false)
  if (!view) return null

  return (
    <div className="flex flex-col gap-3 p-3 lg:h-full lg:flex-row">
      <div className="order-2 lg:order-none lg:contents">
        <TrackerLogSidebar />
      </div>

      <div className="relative order-1 flex min-h-[440px] min-w-0 flex-1 flex-col lg:order-none">
        <TrackerArena />

        <TrackerInputBar />

        <button
          onClick={() => setConfirmLeave(true)}
          className="lift absolute right-3 top-3 z-10 rounded-card border border-subtle bg-card px-3 py-1.5 text-xs font-semibold text-ink-muted shadow-sm hover:text-danger"
        >
          End tracker
        </button>
      </div>

      <div className="order-3 lg:order-none lg:contents">
        <TrackerTeamSidebar />
      </div>

      {confirmLeave && (
        <ConfirmDialog
          title="End this tracker session?"
          message="The current session will be abandoned and you'll return to the new-tracker setup."
          onConfirm={() => {
            setConfirmLeave(false)
            leave()
          }}
          onCancel={() => setConfirmLeave(false)}
        />
      )}
    </div>
  )
}
