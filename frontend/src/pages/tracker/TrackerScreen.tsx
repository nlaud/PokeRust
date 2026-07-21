import { useState } from 'react'
import ConfirmDialog from '../../components/common/ConfirmDialog'
import { useTracker } from '../../store/trackerStore'
import TrackerArena from './TrackerArena'
import TrackerLogSidebar from './TrackerLogSidebar'
import TrackerTeamSidebar from './TrackerTeamSidebar'

/**
 * Tracker mode's screen: the same field/sidebar visual fidelity as battle
 * mode (`TrackerArena` recreates `Arena`, `TrackerTeamSidebar` recreates
 * `TeamInfoSidebar` — both driven by `useTracker` instead of `useBattle`, see
 * their own doc comments), the event log, and a plain multiline text box for
 * entering tracker-syntax events in place of a move selector. The rich inline
 * editor (ghost text, autocomplete, arrow-key event navigation) described in
 * the tracker-mode design doc is a follow-up — this ships the pipeline
 * end-to-end first.
 *
 * A failed submission (parse error or an `apply_information` contradiction)
 * never touches `view`/`log` — see `trackerStore.ts::submitText` — so the
 * turn is already refused; the single red banner below is just that refusal
 * made visible, matching `BattleScreen.tsx`'s error toast exactly.
 */
export default function TrackerScreen() {
  const { view, error, errorLine, clearError, leave, submitText, busy } = useTracker()
  const [text, setText] = useState('')
  const [confirmLeave, setConfirmLeave] = useState(false)
  if (!view) return null

  const submit = async () => {
    if (!text.trim() || busy) return
    const ok = await submitText(text)
    if (ok) setText('')
  }

  return (
    <div className="flex flex-col gap-3 p-3 lg:h-full lg:flex-row">
      <div className="order-2 lg:order-none lg:contents">
        <TrackerLogSidebar />
      </div>

      <div className="relative order-1 flex min-h-[440px] min-w-0 flex-1 flex-col lg:order-none">
        <TrackerArena />

        {/* Floats over the bottom of the arena, glass-soft like
            ControlPanel's move selector — wider, since a multi-line text box
            needs more horizontal room than a button grid. */}
        <div className="glass-soft absolute inset-x-0 bottom-3 z-20 mx-auto w-full max-w-5xl rounded-card p-3 shadow-lg">
          <label className="mb-1 block text-xs font-semibold text-ink-muted">
            Events this turn (end with <code className="rounded bg-subtle px-1">endofturn</code>)
          </label>
          <textarea
            value={text}
            onChange={(e) => setText(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) {
                e.preventDefault()
                void submit()
              }
            }}
            rows={3}
            spellCheck={false}
            placeholder={
              !view.p1?.active.length
                ? 'p leads charizard\no leads tyranitar\nendofturn'
                : 'o1 switch garchomp\np1 thunderbolt o1 45%\nendofturn'
            }
            className="w-full resize-none rounded-card border border-subtle bg-card px-3 py-2 font-mono text-xs"
          />
          <div className="mt-2 flex items-center justify-between">
            <span className="text-[11px] text-ink-muted">Ctrl/Cmd+Enter to submit</span>
            <button
              onClick={() => void submit()}
              disabled={busy || !text.trim()}
              className="lift rounded-card bg-primary px-4 py-1.5 text-sm font-semibold text-white disabled:opacity-40"
            >
              {busy ? 'Applying…' : 'Submit'}
            </button>
          </div>
        </div>

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

      {error && (
        <div className="fixed bottom-4 left-1/2 z-50 flex -translate-x-1/2 items-center gap-3 rounded-card bg-danger px-4 py-2 text-sm font-medium text-white shadow-lg">
          <span>{errorLine !== null ? `Line ${errorLine}: ${error}` : error}</span>
          <button onClick={clearError} className="font-bold" aria-label="Dismiss error">
            ✕
          </button>
        </div>
      )}
    </div>
  )
}
