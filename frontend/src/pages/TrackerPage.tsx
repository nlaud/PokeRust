import { useEffect } from 'react'
import { storedTrackerId, useTracker } from '../store/trackerStore'
import TrackerScreen from './tracker/TrackerScreen'
import TrackerSetupPanel from './tracker/TrackerSetupPanel'

export default function TrackerPage() {
  const { trackerId, view, restore } = useTracker()

  // Reconnect to an in-progress tracker session after a page refresh.
  useEffect(() => {
    if (!trackerId) {
      const stored = storedTrackerId()
      if (stored) void restore(stored)
    }
  }, [trackerId, restore])

  const inSession = !!trackerId && !!view
  return inSession ? <TrackerScreen /> : <TrackerSetupPanel />
}
