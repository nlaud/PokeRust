import { useEffect, useMemo, useRef } from 'react'
import { renderLog, type Tone } from '../../lib/eventText'
import { useTracker } from '../../store/trackerStore'

// Mirrors `pages/simulate/BattleLogSidebar.tsx` exactly, sourced from
// `trackerStore` instead of `battleStore` — `renderLog` (lib/eventText.ts) is
// a pure function of `TurnLogEntry[]`, so tracker mode's log renders through
// the exact same engine as battle mode's, no adaptation needed.
const TONE_CLASSES: Record<Tone, string> = {
  default: 'text-ink',
  muted: 'text-ink-muted',
  success: 'text-success',
  danger: 'text-danger',
  primary: 'text-primary',
  warning: 'text-warning',
}

export default function TrackerLogSidebar() {
  const log = useTracker((s) => s.log)
  const turns = useMemo(() => renderLog(log), [log])
  const scrollRef = useRef<HTMLDivElement>(null)

  useEffect(() => {
    const el = scrollRef.current
    if (el) el.scrollTop = el.scrollHeight
  }, [turns])

  return (
    <aside className="glass flex max-h-64 w-full shrink-0 flex-col rounded-card lg:max-h-none lg:w-72">
      <h2 className="border-b border-subtle px-3 py-2 text-sm font-semibold">Event Log</h2>
      <div ref={scrollRef} className="flex-1 overflow-y-auto p-3">
        {turns.length === 0 && <p className="text-xs text-ink-muted">No events yet.</p>}
        {turns.map((turn, i) => (
          <div key={i} className="mb-3">
            <div className="mb-1 flex items-center gap-2">
              <span className="text-[11px] font-bold uppercase tracking-wide text-ink-muted">
                {turn.label}
              </span>
              <div className="h-px flex-1 bg-subtle" />
            </div>
            {turn.lines.map((line, j) => (
              <p
                key={j}
                className={`text-xs leading-5 ${TONE_CLASSES[line.tone]} ${
                  line.depth > 0 ? 'border-l border-subtle pl-2' : ''
                }`}
                style={line.depth > 0 ? { marginLeft: line.depth * 10 } : undefined}
              >
                {line.text}
              </p>
            ))}
          </div>
        ))}
      </div>
    </aside>
  )
}
